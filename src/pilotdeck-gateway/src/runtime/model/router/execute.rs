//! Live execute half of RouterRuntime. Buffers framing only until the first
//! content event; provider events after that point reach the caller immediately.
use super::super::{
    fallback::{self, error_event, is, object},
    health::ProviderHealthInput,
    http::{self, ModelStreamError, StreamOptions, StreamOutput},
};
use super::*;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};

#[derive(Clone, Default)]
pub struct RouterExecuteContext {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub fallback_models: Vec<RouterModelRef>,
    pub managed_model_policy: Option<ManagedModelPolicy>,
    /// `RouterExecuteContext.projectPath`; persisted on stats records.
    pub project_path: Option<JsString>,
    /// Includes the turn abort signal and provider HTTP timeout options.
    pub stream_options: StreamOptions,
}

#[derive(Debug)]
pub enum RouterExecuteError {
    Router(RouterRuntimeError),
    Model(ModelStreamError),
}
impl From<ModelStreamError> for RouterExecuteError {
    fn from(error: ModelStreamError) -> Self {
        Self::Model(error)
    }
}
impl From<RouterRuntimeError> for RouterExecuteError {
    fn from(error: RouterRuntimeError) -> Self {
        Self::Router(error)
    }
}

#[derive(Default)]
struct Outcome {
    buffered: Vec<JsValue>,
    pending: Vec<JsValue>,
    error: Option<JsValue>,
    usage: Option<JsValue>,
    content: bool,
    any_text: bool,
    finish: bool,
}
impl Outcome {
    fn zero_usage(&self) -> bool {
        self.error.is_none()
            && self.finish
            && !self.any_text
            && self.usage.as_ref().is_none_or(|usage| {
                [
                    "totalTokens",
                    "inputTokens",
                    "outputTokens",
                    "cacheReadTokens",
                    "cacheWriteTokens",
                ]
                .iter()
                .map(|key| field(usage, key).as_number().unwrap_or(0.0))
                .sum::<f64>()
                    == 0.0
            })
    }
}
fn aborted(ctx: &RouterExecuteContext) -> bool {
    ctx.stream_options
        .abort
        .as_ref()
        .is_some_and(AbortFlag::is_aborted)
}
fn content_event(event: &JsValue) -> bool {
    [
        "text_delta",
        "thinking_delta",
        "tool_call_start",
        "tool_call_delta",
        "tool_call_end",
    ]
    .iter()
    .any(|kind| is(field(event, "type"), kind))
}

impl RouterRuntime {
    /// `decide + execute`, sharing the existing decision/materialization path.
    pub async fn stream(
        &self,
        input: &RouterDecisionInput,
        ctx: &RouterExecuteContext,
        sink: &mut (dyn FnMut(JsValue) + Send),
    ) -> Result<(), RouterExecuteError> {
        let decision = self.decide(input).await?;
        self.execute(&decision, &input.request, ctx, sink).await
    }

    pub async fn execute(
        &self,
        decision: &JsValue,
        request: &JsValue,
        ctx: &RouterExecuteContext,
        sink: &mut (dyn FnMut(JsValue) + Send),
    ) -> Result<(), RouterExecuteError> {
        let requested = RouterModelRef::new(
            field(decision, "provider")
                .as_string()
                .cloned()
                .unwrap_or_default(),
            field(decision, "model")
                .as_string()
                .cloned()
                .unwrap_or_default(),
        );
        self.assert_model_allowed(&requested)?;
        if ctx
            .managed_model_policy
            .as_ref()
            .is_some_and(|policy| !managed_model_allowed(policy, &requested))
        {
            return Err(RouterRuntimeError::new(
                "SDK_MANAGED_MODEL_DENIED",
                join_message("SDK managedSettings.models denies model ", &requested, "."),
            )
            .into());
        }
        let allowed = |model: &RouterModelRef| {
            (self.is_model_allowed)(model)
                && ctx
                    .managed_model_policy
                    .as_ref()
                    .is_none_or(|policy| managed_model_allowed(policy, model))
        };
        let sdk_fallback: Vec<_> = ctx
            .fallback_models
            .iter()
            .filter(|model| allowed(model))
            .cloned()
            .collect();
        if !self.enabled && sdk_fallback.is_empty() {
            // Disabled routing ignores requestPatch/subagent mutations while
            // retaining cache scoping, capability cap, and media downgrade.
            let simple = object(&[
                ("provider", JsValue::String(requested.provider.clone())),
                ("model", JsValue::String(requested.model.clone())),
            ]);
            let request = fallback::downgrade_request(
                &self.materialize_request(&simple, request),
                self.invoker.as_ref(),
            )?;
            let outcome = self.stream_attempt(&request, ctx, true, sink).await?;
            if let Some(error) = outcome.error
                && !outcome
                    .buffered
                    .iter()
                    .any(|event| is(field(event, "type"), "error"))
            {
                sink(error_event(error));
            }
            return Ok(());
        }
        let scenario = field(decision, "scenarioType")
            .as_string()
            .and_then(|value| value.to_utf8().ok())
            .unwrap_or_default();
        let fallback = if sdk_fallback.is_empty() {
            self.config.fallback.plan(&scenario)
        } else {
            sdk_fallback
        };
        let base = self.materialize_request(decision, request);
        let required = scenario::collect_required_input_modalities(&request_messages(&base));
        let mut candidates = vec![requested.clone()];
        candidates.extend(fallback);
        let mut unique = Vec::<RouterModelRef>::new();
        for candidate in candidates {
            if allowed(&candidate) && !unique.iter().any(|prior| prior.key() == candidate.key()) {
                unique.push(candidate);
            }
        }
        let (native, downgraded): (Vec<_>, Vec<_>) = unique.into_iter().partition(|candidate| {
            scenario::supports_media_requirements(self.invoker.as_ref(), candidate, &required)
        });
        let plans: Vec<_> = native
            .into_iter()
            .map(|model| (model, false))
            .chain(downgraded.into_iter().map(|model| (model, true)))
            .collect();
        if plans.is_empty() {
            let missing =
                scenario::missing_input_modalities(self.invoker.as_ref(), &requested, &required);
            let joined = |items: &[JsString]| {
                let mut parts = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        parts.push(", ".into());
                    }
                    parts.push(JsValue::String(item.clone()));
                }
                fallback::template(&parts)
            };
            let message = fallback::template(&[
                "Router could not find a configured fallback model for ".into(),
                JsValue::String(requested.provider.clone()),
                "/".into(),
                JsValue::String(requested.model.clone()),
                " that supports required input modalities: ".into(),
                joined(&required)?,
                ". Missing: ".into(),
                joined(if missing.is_empty() {
                    &required
                } else {
                    &missing
                })?,
                ".".into(),
            ])?;
            let error = object(&[
                ("provider", JsValue::String(requested.provider.clone())),
                (
                    "protocol",
                    JsValue::String(self.invoker.get_provider_protocol(&requested.provider)),
                ),
                ("code", "unsupported_modality".into()),
                ("message", message),
                ("retryable", JsValue::Bool(false)),
            ]);
            self.emit_execute(
                ctx,
                "pilotdeck_router_execute_failed",
                &[
                    ("scenarioType", field(decision, "scenarioType").clone()),
                    ("provider", JsValue::String(requested.provider)),
                    ("model", JsValue::String(requested.model)),
                    ("error", error.clone()),
                ],
            );
            sink(error_event(error));
            return Ok(());
        }
        let started_at = crate::runtime::context::prompt::iso_timestamp((self.clock)());
        let mut last_error = None;
        let mut last_attempt = None;
        let mut last_routed: Option<JsObject> = None;
        let mut last_outcome = Outcome::default();
        'attempts: for (index, (attempt, downgrade)) in plans.iter().enumerate() {
            if aborted(ctx) {
                return Ok(());
            }
            let health = ProviderHealthInput {
                session_id: ctx.session_id.clone(),
                provider_id: attempt.provider.clone(),
                provider_generation: self.invoker.get_provider_generation(&attempt.provider),
            };
            if index > 0 && self.health.should_skip(&health) && index < plans.len() - 1 {
                continue;
            }
            let mut routed = decision.as_object().cloned().unwrap_or_default();
            routed.insert("provider".into(), JsValue::String(attempt.provider.clone()));
            routed.insert("model".into(), JsValue::String(attempt.model.clone()));
            if index > 0 {
                routed.insert("resolvedFrom".into(), "fallback".into());
            }
            last_routed = Some(routed.clone());
            let mut attempt_request = self.materialize_request(&JsValue::Object(routed), request);
            if *downgrade {
                attempt_request =
                    fallback::downgrade_request(&attempt_request, self.invoker.as_ref())?;
            }
            last_attempt = Some(attempt.clone());
            if truthy(field(decision, "isSubagent"))
                && let Some(budget) = self
                    .config
                    .auto_orchestrate
                    .as_ref()
                    .and_then(|config| config.subagent_max_tokens)
                    .filter(|budget| *budget != 0.0)
            {
                let messages = request_messages(&attempt_request);
                let estimated = estimate_input(messages).await?;
                if estimated > budget {
                    sink(error_event(object(&[("provider", JsValue::String(attempt.provider.clone())), ("protocol", JsValue::String(self.invoker.get_provider_protocol(&attempt.provider))), ("code", "subagent_budget_exceeded".into()), ("message", JsValue::String(format!("Sub-agent budget exceeded ({} estimated tokens > {} limit).", crate::js_value::number_string(estimated), crate::js_value::number_string(budget)).into())), ("retryable", JsValue::Bool(false)), ("userHint", "Reduce the subagent prompt/context, increase the subagent token budget, or split the task into smaller steps.".into())])));
                    return Ok(());
                }
            }
            let mut zero_attempt = 0.0;
            let mut transient = 0.0;
            loop {
                zero_attempt += 1.0;
                let mut outcome = self
                    .stream_attempt(&attempt_request, ctx, false, sink)
                    .await?;
                if let Some(error) = outcome.error.clone() {
                    last_error = Some(error.clone());
                    self.health.record_failure(&health);
                    let eligible = !outcome.content && fallback::is_fallback_eligible(&error);
                    if eligible && index < plans.len() - 1 {
                        let next = &plans[index + 1].0;
                        self.emit_execute(
                            ctx,
                            "pilotdeck_router_fallback",
                            &[
                                ("scenarioType", field(decision, "scenarioType").clone()),
                                ("attempt", JsValue::Number((index + 1) as f64)),
                                ("fromProvider", JsValue::String(attempt.provider.clone())),
                                ("fromModel", JsValue::String(attempt.model.clone())),
                                ("toProvider", JsValue::String(next.provider.clone())),
                                ("toModel", JsValue::String(next.model.clone())),
                                ("error", error),
                            ],
                        );
                        continue 'attempts;
                    }
                    if eligible {
                        let after = field(&error, "retryAfterMs").as_number();
                        let random = if after.is_none()
                            && self.config.transient_retry.enabled
                            && transient < self.config.transient_retry.max_attempts
                        {
                            (ctx.stream_options.random)()
                        } else {
                            0.0
                        };
                        let retry = policy::retry_decide(
                            &self.config,
                            "transient",
                            transient,
                            after,
                            random,
                        );
                        if retry.retry {
                            let delay = http::js_round(retry.delay_ms);
                            self.emit_execute(
                                ctx,
                                "pilotdeck_router_transient_retry",
                                &[
                                    ("attempt", JsValue::Number(transient + 1.0)),
                                    ("delayMs", JsValue::Number(delay)),
                                    ("provider", JsValue::String(attempt.provider.clone())),
                                    ("model", JsValue::String(attempt.model.clone())),
                                    ("errorCode", field(&error, "code").clone()),
                                ],
                            );
                            let reason = if is(field(&error, "code"), "rate_limit_error") {
                                "rate_limit"
                            } else if is(field(&error, "code"), "overloaded_error") {
                                "overloaded"
                            } else if is(field(&error, "code"), "network_error")
                                || is(field(&error, "code"), "timeout")
                            {
                                "network_error"
                            } else {
                                "server_error"
                            };
                            self.retry_progress(
                                ctx,
                                attempt,
                                transient + 1.0,
                                retry.max_attempts,
                                delay,
                                reason,
                            );
                            http::delay(retry.delay_ms, &ctx.stream_options).await?;
                            transient += 1.0;
                            continue;
                        }
                    }
                    for queued in outcome.pending.drain(..) {
                        sink(queued);
                    }
                    // The drained events are TS yields; the terminal failure
                    // handling stays behind the pulls that consume them.
                    ctx.stream_options.yield_boundary().await;
                    last_outcome = outcome;
                    break 'attempts;
                }
                let retry =
                    policy::retry_decide(&self.config, "zero_usage", zero_attempt, None, 0.0);
                if !outcome.content && outcome.zero_usage() && retry.retry {
                    self.emit_execute(
                        ctx,
                        "pilotdeck_router_zero_usage_retry",
                        &[
                            ("attempt", JsValue::Number(zero_attempt)),
                            ("provider", JsValue::String(attempt.provider.clone())),
                            ("model", JsValue::String(attempt.model.clone())),
                        ],
                    );
                    self.retry_progress(
                        ctx,
                        attempt,
                        zero_attempt,
                        retry.max_attempts,
                        retry.delay_ms,
                        "zero_usage",
                    );
                    http::delay(retry.delay_ms, &ctx.stream_options).await?;
                    continue;
                }
                self.health.record_success(&health);
                for queued in outcome.pending.drain(..) {
                    sink(queued);
                }
                // `observeRequest` runs on the pull after the last queued
                // event, like the TS generator resuming past its yields.
                ctx.stream_options.yield_boundary().await;
                let ended_at = crate::runtime::context::prompt::iso_timestamp((self.clock)());
                let usage = final_usage(&attempt_request, &outcome).await?;
                self.cache_session_usage(&ctx.session_id, &usage);
                self.observe_request(
                    ctx,
                    decision,
                    last_routed.as_ref(),
                    attempt,
                    &usage,
                    (&started_at, &ended_at),
                );
                return Ok(());
            }
        }
        if let (Some(error), Some(attempt)) = (last_error, last_attempt) {
            self.emit_execute(
                ctx,
                "pilotdeck_router_execute_failed",
                &[
                    ("scenarioType", field(decision, "scenarioType").clone()),
                    ("provider", JsValue::String(attempt.provider.clone())),
                    ("model", JsValue::String(attempt.model.clone())),
                    ("error", error.clone()),
                ],
            );
            let ended_at = crate::runtime::context::prompt::iso_timestamp((self.clock)());
            // Rust-specific failures (`cpu::run` JoinError at shutdown, or an
            // over-limit buffered string) must not swallow the buffered events
            // and terminal error event. Degrade to "no usage reported".
            let fail_usage = final_usage(request, &last_outcome)
                .await
                .unwrap_or_else(|_| JsValue::Object(JsObject::new()));
            self.observe_request(
                ctx,
                decision,
                last_routed.as_ref(),
                &attempt,
                &fail_usage,
                (&started_at, &ended_at),
            );
            if !last_outcome.content {
                for event in last_outcome.buffered {
                    if !is(field(&event, "type"), "error") {
                        sink(event);
                    }
                }
                // The buffered events are TS yields; the terminal error event
                // stays behind the pulls that consume them.
                ctx.stream_options.yield_boundary().await;
            }
            let mut error = error.as_object().cloned().unwrap_or_default();
            error.insert("provider".into(), JsValue::String(attempt.provider));
            error.insert("model".into(), JsValue::String(attempt.model));
            sink(error_event(JsValue::Object(error)));
        }
        Ok(())
    }

    /// `usageObserver.observeRequest`: one completed routed attempt (success or
    /// terminal failure). Property order matches the TS record literal followed
    /// by `TokenStatsCollector.observe`'s `cost`, `costSource`, `baselineCost`.
    fn observe_request(
        &self,
        ctx: &RouterExecuteContext,
        decision: &JsValue,
        routed: Option<&JsObject>,
        attempt: &RouterModelRef,
        usage: &JsValue,
        timestamps: (&str, &str),
    ) {
        let (started_at, ended_at) = timestamps;
        let mut record = JsObject::new();
        record.insert("sessionId".into(), JsValue::String(ctx.session_id.clone()));
        if !ctx.turn_id.is_empty() {
            record.insert("turnId".into(), JsValue::String(ctx.turn_id.clone()));
        }
        if let Some(project_path) = ctx.project_path.as_ref().filter(|value| !value.is_empty()) {
            record.insert("projectPath".into(), JsValue::String(project_path.clone()));
        }
        record.insert(
            "scenarioType".into(),
            field(decision, "scenarioType").clone(),
        );
        if let Some(resolved_from) = routed.and_then(|routed| routed.get("resolvedFrom")) {
            record.insert("resolvedFrom".into(), resolved_from.clone());
        }
        record.insert("provider".into(), JsValue::String(attempt.provider.clone()));
        record.insert("model".into(), JsValue::String(attempt.model.clone()));
        if let Some(tier) = field(decision, "tokenSaverTier")
            .as_string()
            .filter(|value| !value.is_empty())
        {
            record.insert("tier".into(), JsValue::String(tier.clone()));
        }
        record.insert(
            "role".into(),
            JsValue::String(
                if truthy(field(decision, "isSubagent")) {
                    "subagent"
                } else {
                    "main"
                }
                .into(),
            ),
        );
        record.insert("usage".into(), usage.clone());
        record.insert("startedAt".into(), JsValue::String(started_at.into()));
        record.insert("endedAt".into(), JsValue::String(ended_at.into()));
        let mut record = JsValue::Object(record);
        self.stats
            .lock()
            .expect("router stats")
            .observe(&mut record);
    }

    async fn stream_attempt(
        &self,
        request: &JsValue,
        ctx: &RouterExecuteContext,
        passthrough: bool,
        sink: &mut (dyn FnMut(JsValue) + Send),
    ) -> Result<Outcome, ModelStreamError> {
        let mut outcome = Outcome::default();
        let saw_aborted_event = AtomicBool::new(false);
        let mut receive = |output| match output {
            StreamOutput::RetryProgress(progress) => self.emit_execute(
                ctx,
                "pilotdeck_router_retry_progress",
                &[
                    ("attempt", field(&progress, "attempt").clone()),
                    ("maxAttempts", field(&progress, "maxAttempts").clone()),
                    ("delayMs", field(&progress, "delayMs").clone()),
                    ("reason", field(&progress, "reason").clone()),
                    ("provider", field(&progress, "provider").clone()),
                    ("model", field(&progress, "model").clone()),
                ],
            ),
            StreamOutput::Event(event) => {
                if aborted(ctx) {
                    saw_aborted_event.store(true, Ordering::SeqCst);
                    return;
                }
                let kind = field(&event, "type");
                if is(kind, "text_delta") && truthy(field(&event, "text"))
                    || is(kind, "tool_call_delta") && truthy(field(&event, "delta"))
                    || is(kind, "tool_call_end")
                {
                    outcome.any_text = true;
                }
                if is(kind, "message_end") {
                    outcome.finish = true;
                }
                if is(kind, "usage") {
                    outcome.usage = Some(field(&event, "usage").clone());
                }
                if is(kind, "error") {
                    outcome.error = Some(field(&event, "error").clone());
                }
                outcome.buffered.push(event.clone());
                if passthrough || outcome.content {
                    sink(event);
                } else if content_event(&event) {
                    for queued in outcome.pending.drain(..) {
                        sink(queued);
                    }
                    sink(event);
                    outcome.content = true;
                } else {
                    outcome.pending.push(event);
                }
            }
        };
        let result = {
            let mut stream = self
                .invoker
                .stream(request, &ctx.stream_options, &mut receive);
            // A provider yielding after cancellation must be dropped, even if
            // it then remains pending. This is an event observation, not a
            // second response/timeout race; HTTP owns those races.
            std::future::poll_fn(|cx| {
                let result = stream.as_mut().poll(cx);
                if saw_aborted_event.load(Ordering::SeqCst) {
                    Poll::Ready(Err(fallback::abort_error()))
                } else {
                    result
                }
            })
            .await
        };
        if let Err(error) = result {
            if aborted(ctx) {
                return Err(error);
            }
            let provider = field(request, "provider")
                .as_string()
                .cloned()
                .unwrap_or_default();
            outcome.error = Some(fallback::canonical_error(
                error,
                request,
                self.invoker.get_provider_protocol(&provider),
            ));
        }
        Ok(outcome)
    }

    fn emit_execute(&self, ctx: &RouterExecuteContext, kind: &str, fields: &[(&str, JsValue)]) {
        let mut event = JsObject::new();
        event.insert("type".into(), kind.into());
        event.insert("sessionId".into(), JsValue::String(ctx.session_id.clone()));
        event.insert("turnId".into(), JsValue::String(ctx.turn_id.clone()));
        for (key, value) in fields {
            event.insert((*key).into(), value.clone());
        }
        self.events.emit(JsValue::Object(event));
    }
    fn retry_progress(
        &self,
        ctx: &RouterExecuteContext,
        attempt: &RouterModelRef,
        count: f64,
        max: f64,
        delay: f64,
        reason: &str,
    ) {
        self.emit_execute(
            ctx,
            "pilotdeck_router_retry_progress",
            &[
                ("attempt", JsValue::Number(count)),
                ("maxAttempts", JsValue::Number(max)),
                ("delayMs", JsValue::Number(delay)),
                ("reason", reason.into()),
                ("provider", JsValue::String(attempt.provider.clone())),
                ("model", JsValue::String(attempt.model.clone())),
            ],
        );
    }
}

async fn estimate_input(messages: Vec<JsValue>) -> Result<f64, ModelStreamError> {
    super::super::cpu::run(move || {
        Ok::<_, crate::host::HostError>(policy::estimate_input_tokens(&messages))
    })
    .await
    .map_err(|error| ModelStreamError::Thrown {
        name: "Error".into(),
        message: error.message.into(),
    })
}
async fn final_usage(request: &JsValue, outcome: &Outcome) -> Result<JsValue, ModelStreamError> {
    if let Some(usage) = &outcome.usage
        && (truthy(field(usage, "inputTokens")) || truthy(field(usage, "outputTokens")))
    {
        return Ok(usage.clone());
    }
    let input = estimate_input(request_messages(request)).await?;
    let mut units = Vec::new();
    for event in &outcome.buffered {
        let text = if is(field(event, "type"), "text_delta")
            || is(field(event, "type"), "thinking_delta")
        {
            field(event, "text")
        } else if is(field(event, "type"), "tool_call_delta") {
            field(event, "delta")
        } else {
            continue;
        };
        if let Some(text) = text.as_string() {
            http::append_js_string(&mut units, text.units())?;
        }
    }
    let output = super::super::cpu::run(move || {
        Ok::<_, crate::host::HostError>(
            pilotdeck_tokenizer::count_tokens(&JsString::from_utf16(units)).unwrap_or(0) as f64,
        )
    })
    .await
    .map_err(|error| ModelStreamError::Thrown {
        name: "Error".into(),
        message: error.message.into(),
    })?;
    Ok(object(&[
        ("inputTokens", JsValue::Number(input)),
        ("outputTokens", JsValue::Number(output)),
        ("totalTokens", JsValue::Number(input + output)),
    ]))
}
