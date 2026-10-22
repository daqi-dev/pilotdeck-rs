use crate::{AdvertisedMethods, HostExchange, model_admission as a};
use pilotdeck_agent::ProjectedMessage;
use pilotdeck_agent::ports::*;
use pilotdeck_protocol::model::{BoundarySnapshot, ToolSchema, Usage};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    wire::{EnvelopeRef, MessageRef},
};

fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert(k.into(), v);
    }
    o
}
fn put(o: &mut JsObject, k: &str, v: JsValue) {
    o.insert(k.into(), v);
}
fn opt<T>(o: &mut JsObject, k: &str, v: Option<T>, f: impl FnOnce(T) -> JsValue) {
    if let Some(v) = v {
        put(o, k, f(v));
    }
}
fn s(v: &JsString) -> JsValue {
    JsValue::String(v.clone())
}
fn n(v: f64) -> JsValue {
    JsValue::Number(v)
}
fn b(v: bool) -> JsValue {
    JsValue::Bool(v)
}
fn boundary(v: &BoundarySnapshot) -> JsValue {
    v.value().clone()
}
fn messages(v: &[ProjectedMessage]) -> JsValue {
    JsValue::Array(v.iter().map(|m| boundary(&m.boundary)).collect())
}
fn strings(v: &[JsString]) -> JsValue {
    JsValue::Array(v.iter().map(s).collect())
}
fn schemas(v: &[ToolSchema]) -> JsValue {
    JsValue::Array(
        v.iter()
            .map(|t| {
                let o = obj([
                    ("name", s(&t.name)),
                    ("description", optional_literal(t.description.as_ref(), s)),
                    ("inputSchema", JsValue::Object(t.input_schema.clone())),
                ]);
                JsValue::Object(o)
            })
            .collect(),
    )
}
fn usage(v: &Usage) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in [
        ("inputTokens", v.input_tokens),
        ("outputTokens", v.output_tokens),
        ("cacheReadTokens", v.cache_read_tokens),
        ("cacheWriteTokens", v.cache_write_tokens),
        ("totalTokens", v.total_tokens),
        ("nativeCost", v.native_cost),
    ] {
        opt(&mut o, k, v, n)
    }
    JsValue::Object(o)
}
fn calibration(v: &TokenCalibration) -> JsValue {
    JsValue::Object(obj([
        ("provider", s(&v.route.provider)),
        ("model", s(&v.route.model)),
        ("actualInputTokens", n(v.actual_input_tokens)),
        ("estimatedInputTokens", n(v.estimated_input_tokens)),
    ]))
}
fn protocol(v: pilotdeck_protocol::model::ModelProtocol) -> JsValue {
    JsValue::from(match v {
        pilotdeck_protocol::model::ModelProtocol::Anthropic => "anthropic",
        pilotdeck_protocol::model::ModelProtocol::OpenAi => "openai",
        pilotdeck_protocol::model::ModelProtocol::OpenAiResponses => "openai-responses",
        pilotdeck_protocol::model::ModelProtocol::Google => "google",
    })
}
fn model_hints(h: &ModelHints) -> JsValue {
    let mut o = obj([
        ("sessionId", s(&h.session_id)),
        ("turnId", s(&h.turn_id)),
        ("runId", s(&h.execution.run_id)),
        ("operationId", s(&h.execution.operation_id)),
    ]);
    opt(
        &mut o,
        "idempotencyKey",
        h.execution.idempotency_key.as_ref(),
        s,
    );
    opt(
        &mut o,
        "operationDeadline",
        h.execution.operation_deadline.as_ref(),
        s,
    );
    opt(&mut o, "workspaceId", h.workspace_id.as_ref(), s);
    opt(
        &mut o,
        "storageConfigVersion",
        h.storage_config_version.as_ref(),
        s,
    );
    opt(&mut o, "metadata", h.metadata.as_ref(), |m| {
        JsValue::Object(m.clone())
    });
    opt(&mut o, "modelOverride", h.model_override.as_ref(), |r| {
        JsValue::Object(obj([("provider", s(&r.provider)), ("model", s(&r.model))]))
    });
    JsValue::Object(o)
}
fn optional_literal<T>(v: Option<T>, f: impl FnOnce(T) -> JsValue) -> JsValue {
    v.map_or(JsValue::Undefined, f)
}
fn execution(h: &ToolHints) -> JsValue {
    JsValue::Object(obj([
        ("runId", s(&h.execution.run_id)),
        ("turnId", s(&h.turn_id)),
        ("operationId", s(&h.execution.operation_id)),
        (
            "idempotencyKey",
            optional_literal(h.execution.idempotency_key.as_ref(), s),
        ),
        (
            "operationDeadline",
            optional_literal(h.execution.operation_deadline.as_ref(), s),
        ),
    ]))
}
fn tool_hints(h: &ToolHints) -> JsValue {
    let mut o = obj([
        ("sessionId", s(&h.session_id)),
        ("turnId", s(&h.turn_id)),
        ("cwd", s(&h.cwd)),
        (
            "permissionMode",
            JsValue::from(match h.permission_mode {
                PermissionMode::Default => "default",
                PermissionMode::Plan => "plan",
                PermissionMode::BypassPermissions => "bypassPermissions",
            }),
        ),
        (
            "permissionContext",
            h.context_extensions
                .get("permissionContext")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        ("runMode", optional_literal(h.run_mode.as_ref(), s)),
        (
            "currentToolCallId",
            h.context_extensions
                .get("currentToolCallId")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "maxResultBytes",
            h.context_extensions
                .get("maxResultBytes")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
    ]);
    if let Some(v) = h.context_extensions.get("outputTruncated") {
        put(&mut o, "outputTruncated", v.clone());
    }
    JsValue::Object(o)
}
fn permission_hints(h: &ToolHints) -> JsValue {
    let full = tool_hints(h);
    let mut o = JsObject::new();
    for k in [
        "sessionId",
        "turnId",
        "cwd",
        "permissionMode",
        "permissionContext",
        "runMode",
        "currentToolCallId",
    ] {
        put(
            &mut o,
            k,
            full.get(k).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    JsValue::Object(o)
}
// Source AgentLoop createContextPrepareInput, then createModelRequest's preview
// spread, then hostContextRuntime serializeContextInput. Native abortSignal is
// deliberately outside the serializable payload and stays with CallControl.
#[derive(Clone, Copy)]
enum ContextPurpose {
    ModelRequest,
    BudgetProjection,
}
fn context_prepare(v: &ContextPrepare) -> JsValue {
    context_input(v, ContextPurpose::ModelRequest)
}
fn context_input(v: &ContextPrepare, purpose: ContextPurpose) -> JsValue {
    let mut o = obj([
        ("sessionId", s(&v.session_id)),
        ("turnId", s(&v.turn_id)),
        ("cwd", s(&v.cwd)),
        (
            "runtimeContextSurface",
            optional_literal(v.runtime_context_surface, |x| {
                JsValue::from(match x {
                    RuntimeContextSurface::SystemPrompt => "system_prompt",
                    RuntimeContextSurface::UserMessage => "user_message",
                })
            }),
        ),
        ("provider", s(&v.route.provider)),
        ("model", s(&v.route.model)),
        ("protocol", optional_literal(v.protocol, protocol)),
        (
            "supportsPromptCache",
            optional_literal(v.supports_prompt_cache, b),
        ),
        ("permissionMode", s(&v.permission_mode)),
        ("runMode", optional_literal(v.run_mode.as_ref(), s)),
        (
            "additionalWorkingDirectories",
            strings(&v.additional_working_directories),
        ),
        ("messages", messages(&v.messages)),
        ("tools", schemas(&v.tools)),
        ("maxMessages", optional_literal(v.max_messages, n)),
        (
            "customSystemPrompt",
            optional_literal(v.custom_system_prompt.as_ref(), s),
        ),
        (
            "appendSystemPrompt",
            optional_literal(v.append_system_prompt.as_ref(), s),
        ),
    ]);
    // Normal requests append an own previewOnly slot. createBudgetPreparation
    // does not; an explicitly supplied budget-preview value remains present.
    if matches!(purpose, ContextPurpose::ModelRequest) || v.preview_only.is_some() {
        put(&mut o, "previewOnly", optional_literal(v.preview_only, b));
    }
    // AgentLoop doesn't supply stepId. The durable wrapper appends it after
    // spreading input; don't fabricate an own Undefined stepId on normal calls.
    opt(&mut o, "stepId", v.step_id, n);
    JsValue::Object(o)
}
fn invocation(v: &ToolInvocation) -> JsValue {
    JsValue::Object(obj([
        ("name", s(&v.name)),
        ("arguments", v.arguments.clone()),
        ("toolCallId", s(&v.tool_call_id)),
    ]))
}
fn compact_input(v: &CompactionIntent) -> JsValue {
    let stage = match v.stage {
        BudgetStage::PreRoute => "pre_route",
        BudgetStage::Routed => "routed",
        BudgetStage::Recovery => "recovery",
    };
    let trigger = match v.trigger {
        CompactionTrigger::Auto => "auto",
        CompactionTrigger::Reactive => "reactive",
        CompactionTrigger::Manual => "manual",
    };
    let mut o = obj([
        ("messages", messages(&v.messages)),
        ("trigger", JsValue::from(trigger)),
    ]);
    opt(&mut o, "sessionId", v.session_id.as_ref(), s);
    opt(&mut o, "turnId", v.turn_id.as_ref(), s);
    for (k, value) in [
        ("maxContextTokens", v.max_context_tokens.map(n)),
        ("reservedOutputTokens", v.reserved_output_tokens.map(n)),
        ("manualForce", v.manual_force.map(b)),
        ("allowFallbackOnFailure", v.allow_fallback_on_failure.map(b)),
    ] {
        if let Some(value) = value {
            put(&mut o, k, value);
        }
    }
    opt(&mut o, "budgetRequest", v.budget_request.as_ref(), |r| {
        r.wire.request().clone()
    });
    opt(&mut o, "budgetPreparation", v.preparation.as_ref(), |v| {
        context_input(v, ContextPurpose::BudgetProjection)
    });
    opt(
        &mut o,
        "budgetPreparationId",
        v.prepared_handle.as_ref(),
        |h| s(&h.0),
    );
    opt(
        &mut o,
        "budgetCalibration",
        v.calibration.as_ref(),
        calibration,
    );
    let mut intent = o.clone();
    put(&mut intent, "budgetStage", JsValue::from(stage));
    let projection =
        pilotdeck_protocol::compaction::project_compaction_budget(&JsValue::Object(intent));
    // hostContextRuntime carries the rich request/preparation beside projection.
    let mut serialized = JsObject::new();
    for (key, value) in projection.as_object().unwrap().entries() {
        if !["request", "preparation", "calibration"]
            .iter()
            .any(|name| key.equals_str(name))
        {
            serialized.insert(key.clone(), value.clone());
        }
    }
    put(&mut o, "budgetProjection", JsValue::Object(serialized));
    JsValue::Object(o)
}
fn payload(call: &HostCall) -> JsObject {
    let mut o = obj([("operation", JsValue::from(call.method().1))]);
    match call {
        HostCall::ModelMetadata { route } => {
            put(&mut o, "provider", s(&route.provider));
            put(&mut o, "model", s(&route.model));
        }
        HostCall::ModelPrepare {
            handle,
            request,
            hints,
        }
        | HostCall::ModelPull {
            handle,
            request,
            hints,
        } => {
            put(&mut o, "request", request.wire.request().clone());
            put(&mut o, "preparationId", s(&handle.0));
            put(&mut o, "context", model_hints(hints));
        }
        HostCall::ModelMaterialize { handle, request } => {
            put(&mut o, "preparationId", s(&handle.0));
            put(&mut o, "request", request.wire.request().clone());
        }
        HostCall::ModelClose { handle } => put(&mut o, "preparationId", s(&handle.0)),
        HostCall::ModelEagerStream {
            handle,
            request,
            hints,
        } => {
            put(
                &mut o,
                "request",
                optional_literal(request.as_ref(), |r| r.wire.request().clone()),
            );
            put(&mut o, "preparationId", s(&handle.0));
            put(&mut o, "context", model_hints(hints));
        }
        HostCall::BudgetEstimate { request } => {
            put(&mut o, "request", request.wire.request().clone())
        }
        HostCall::BudgetEvaluate { request, options } => {
            put(&mut o, "request", request.wire.request().clone());
            let mut options_o = obj([("maxContextTokens", n(options.max_context_tokens))]);
            opt(
                &mut options_o,
                "reservedOutputTokens",
                options.reserved_output_tokens,
                n,
            );
            opt(
                &mut options_o,
                "useProviderCount",
                options.use_provider_count,
                b,
            );
            opt(
                &mut options_o,
                "calibration",
                options.calibration.as_ref(),
                calibration,
            );
            put(&mut o, "options", JsValue::Object(options_o));
        }
        HostCall::BudgetCost { usage: u, route } => {
            put(&mut o, "usage", optional_literal(u.as_ref(), usage));
            put(&mut o, "provider", s(&route.provider));
            put(&mut o, "model", s(&route.model));
        }
        HostCall::BudgetCostSnapshot { usage: u, route } => {
            put(
                &mut o,
                "usage",
                u.as_ref().map_or(JsValue::Undefined, boundary),
            );
            put(&mut o, "provider", s(&route.provider));
            put(&mut o, "model", s(&route.model));
        }
        HostCall::TurnDrain | HostCall::TurnDrainOrClose | HostCall::ListTools => {}
        HostCall::TurnPersistCompaction {
            boundary: v,
            messages: m,
        } => {
            put(&mut o, "boundary", boundary(&v.boundary));
            put(&mut o, "messages", messages(m));
        }
        HostCall::ExecuteBatch { calls, hints } => {
            put(
                &mut o,
                "calls",
                JsValue::Array(calls.iter().map(invocation).collect()),
            );
            put(&mut o, "context", tool_hints(hints));
            put(&mut o, "execution", execution(hints));
        }
        HostCall::ExecuteSingle { call, hints } => {
            put(&mut o, "name", s(&call.name));
            put(&mut o, "arguments", call.arguments.clone());
            put(&mut o, "toolCallId", s(&call.tool_call_id));
            put(&mut o, "context", tool_hints(hints));
            put(&mut o, "execution", execution(hints));
        }
        HostCall::PlanTodo(v) => {
            let method = match &v.mutation {
                PlanTodoMutation::Read => "read",
                PlanTodoMutation::MarkPlanApproved { .. } => "mark_plan_approved",
                PlanTodoMutation::RecordTodoWrite { .. } => "record_todo_write",
                PlanTodoMutation::WriteTodos { .. } => "write_todos",
                PlanTodoMutation::MarkToolProgress { .. } => "mark_tool_progress",
            };
            put(&mut o, "method", JsValue::from(method));
            put(&mut o, "sessionId", s(&v.session_id));
            put(&mut o, "turnId", s(&v.turn_id));
            match &v.mutation {
                PlanTodoMutation::Read => {}
                PlanTodoMutation::MarkPlanApproved { plan } => put(&mut o, "plan", s(plan)),
                PlanTodoMutation::RecordTodoWrite {
                    markdown,
                    todos,
                    reason,
                } => {
                    put(&mut o, "markdown", s(markdown));
                    put(
                        &mut o,
                        "todos",
                        JsValue::Array(todos.iter().map(|t| boundary(&t.boundary)).collect()),
                    );
                    if let Some(r) = reason.as_ref().filter(|r| !r.is_empty()) {
                        put(&mut o, "reason", s(r));
                    }
                }
                PlanTodoMutation::WriteTodos {
                    todos,
                    markdown,
                    merge,
                    reason,
                } => {
                    put(
                        &mut o,
                        "todos",
                        JsValue::Array(todos.iter().map(|t| boundary(&t.boundary)).collect()),
                    );
                    opt(&mut o, "markdown", markdown.as_ref(), s);
                    if *merge == Some(true) {
                        put(&mut o, "merge", b(true));
                    }
                    if let Some(r) = reason.as_ref().filter(|r| !r.is_empty()) {
                        put(&mut o, "reason", s(r));
                    }
                }
                PlanTodoMutation::MarkToolProgress { tool_name } => {
                    put(&mut o, "toolName", s(tool_name))
                }
            }
        }
        HostCall::DecidePermission {
            tool,
            input,
            hints,
            call_id,
        } => {
            let t = &tool.view;
            let mut descriptor = obj([
                ("name", s(&t.name)),
                ("description", s(&t.description)),
                ("kind", s(&t.kind)),
            ]);
            opt(
                &mut descriptor,
                "requiredRuntimeCapabilities",
                t.required_runtime_capabilities.as_ref(),
                |v| strings(v),
            );
            put(
                &mut descriptor,
                "inputSchema",
                JsValue::Object(t.input_schema.clone()),
            );
            put(&mut descriptor, "readOnly", b(t.read_only));
            put(
                &mut descriptor,
                "requiresUserInteraction",
                b(t.requires_user_interaction),
            );
            put(&mut o, "tool", JsValue::Object(descriptor));
            put(&mut o, "input", input.clone());
            put(&mut o, "context", permission_hints(hints));
            put(&mut o, "toolCallId", optional_literal(call_id.as_ref(), s));
        }
        HostCall::PrepareContext(v) => put(&mut o, "input", context_prepare(v)),
        HostCall::ApplyToolResults(v) => {
            let mut i = obj([
                ("sessionId", s(&v.session_id)),
                ("turnId", s(&v.turn_id)),
                (
                    "toolResultMessage",
                    boundary(&v.tool_result_message.boundary),
                ),
                ("messages", messages(&v.messages)),
            ]);
            opt(
                &mut i,
                "supplementalMessages",
                v.supplemental_messages.as_ref(),
                |m| {
                    JsValue::Array(
                        m.iter()
                            .map(|s| {
                                JsValue::Object(obj([
                                    ("toolCallId", JsValue::String(s.tool_call_id.clone())),
                                    ("message", boundary(&s.message.boundary)),
                                ]))
                            })
                            .collect(),
                    )
                },
            );
            put(&mut o, "input", JsValue::Object(i));
        }
        HostCall::RecoverModelError(v) => put(
            &mut o,
            "input",
            JsValue::Object(obj([
                ("sessionId", s(&v.session_id)),
                ("turnId", s(&v.turn_id)),
                ("error", boundary(&v.error.boundary)),
                ("messages", messages(&v.messages)),
                ("hasAttemptedCompact", b(v.has_attempted_compact)),
            ])),
        ),
        HostCall::RecoverModelErrorBoundary(v) => put(
            &mut o,
            "input",
            JsValue::Object(obj([
                ("sessionId", s(&v.session_id)),
                ("turnId", s(&v.turn_id)),
                ("error", boundary(&v.error)),
                ("messages", messages(&v.messages)),
                ("hasAttemptedCompact", b(v.has_attempted_compact)),
            ])),
        ),
        HostCall::CaptureTurn(v) => put(
            &mut o,
            "input",
            JsValue::Object(obj([
                ("sessionId", s(&v.session_id)),
                ("turnId", s(&v.turn_id)),
                ("messages", messages(&v.messages)),
                ("errored", b(v.errored)),
            ])),
        ),
        HostCall::AutoCompact(v) => put(&mut o, "input", compact_input(v)),
        HostCall::DispatchLifecycle { event, payload } => {
            put(&mut o, "event", s(event));
            opt(&mut o, "payload", payload.as_ref(), |p| {
                JsValue::Object(p.clone())
            });
        }
        HostCall::EmitEvent { event } => put(&mut o, "event", boundary(event)),
    };
    o
}
fn capture<T>(v: &JsValue, view: T) -> Captured<T> {
    Captured {
        view,
        boundary: BoundarySnapshot::capture(v),
    }
}
fn nonnegative(v: &JsValue) -> PortResult<f64> {
    let n = a::number(v)?;
    if !n.is_finite() || n < 0.0 {
        return Err(a::invalid("Budget number must be non-negative finite"));
    }
    Ok(n)
}
fn label(v: &JsValue, k: &str, choices: &[&str]) -> PortResult<usize> {
    let name = a::str_field(v, k)?;
    choices
        .iter()
        .position(|c| name.equals_str(c))
        .ok_or_else(|| a::invalid(format!("Invalid {k}")))
}
// Fixed default factory readModelMetadataSnapshot, not the host dispatcher’s
// similarly named metadata serializer. Keep source field/error precedence.
fn metadata_integer(v: &JsValue, field: &str) -> PortResult<f64> {
    positive_safe(Some(v)).ok_or_else(|| {
        a::invalid(format!(
            "Host model metadata response contains an invalid {field}."
        ))
    })
}
fn model_metadata(v: &JsValue) -> PortResult<ModelMetadata> {
    let provider = v
        .get("provider")
        .and_then(JsValue::as_string)
        .filter(|s| !s.is_empty());
    let model = v
        .get("model")
        .and_then(JsValue::as_string)
        .filter(|s| !s.is_empty());
    let (Some(provider), Some(model)) = (provider, model) else {
        return Err(a::invalid(
            "Host model metadata response must include provider and model.",
        ));
    };
    let route = Route {
        provider: provider.clone(),
        model: model.clone(),
    };
    let max_context_tokens = a::optional(v, "maxContextTokens", |v| {
        metadata_integer(v, "maxContextTokens")
    })?;
    let max_output_tokens = a::optional(v, "maxOutputTokens", |v| {
        metadata_integer(v, "maxOutputTokens")
    })?;
    let token_limits = match v.get("tokenLimits").and_then(JsValue::as_object) {
        Some(t) => Some(TokenLimits {
            max_context_tokens: metadata_integer(
                t.get("maxContextTokens").unwrap_or(&JsValue::Undefined),
                "tokenLimits.maxContextTokens",
            )?,
            max_output_tokens: match t.get("maxOutputTokens") {
                None | Some(JsValue::Undefined) => None,
                Some(v) => Some(metadata_integer(v, "tokenLimits.maxOutputTokens")?),
            },
        }),
        None => None,
    };
    let protocol = a::optional(v, "protocol", |v| {
        a::protocol(v)
            .map_err(|_| a::invalid("Host model metadata response contains an invalid protocol."))
    })?;
    let supports_prompt_cache = a::optional(v, "supportsPromptCache", |v| {
        v.as_bool().ok_or_else(|| {
            a::invalid("Host model metadata response contains an invalid prompt-cache capability.")
        })
    })?;
    Ok(ModelMetadata {
        route,
        max_context_tokens,
        max_output_tokens,
        token_limits,
        protocol,
        supports_prompt_cache,
    })
}
fn budget(v: &JsValue) -> PortResult<BudgetSnapshot> {
    a::object(v)?;
    Ok(BudgetSnapshot {
        tokens: nonnegative(a::field(v, "tokens")?)?,
        max_context_tokens: nonnegative(a::field(v, "maxContextTokens")?)?,
        warning_ratio: nonnegative(a::field(v, "warningRatio")?)?,
        blocking_ratio: nonnegative(a::field(v, "blockingRatio")?)?,
        ratio: nonnegative(a::field(v, "ratio")?)?,
        state: match label(v, "state", &["ok", "warning", "blocking"])? {
            0 => BudgetState::Ok,
            1 => BudgetState::Warning,
            _ => BudgetState::Blocking,
        },
        local_estimate_tokens: a::opt_number(v, "localEstimateTokens").ok().flatten(),
        display_tokens: a::opt_number(v, "displayTokens").ok().flatten(),
        estimate_source: a::optional(v, "estimateSource", |x| {
            Ok(if a::string(x)?.equals_str("estimator") {
                EstimateSource::Estimator
            } else if a::string(x)?.equals_str("usage") {
                EstimateSource::Usage
            } else {
                return Err(a::invalid("Invalid estimateSource"));
            })
        })
        .ok()
        .flatten(),
        usage_tokens: a::opt_number(v, "usageTokens").ok().flatten(),
        calibration_actual_input_tokens: a::opt_number(v, "calibrationActualInputTokens")
            .ok()
            .flatten(),
        calibration_estimated_input_tokens: a::opt_number(v, "calibrationEstimatedInputTokens")
            .ok()
            .flatten(),
        total_context_tokens: a::opt_number(v, "totalContextTokens").ok().flatten(),
        effective_context_tokens: a::opt_number(v, "effectiveContextTokens").ok().flatten(),
        max_output_tokens: a::opt_number(v, "maxOutputTokens").ok().flatten(),
        source: a::optional(v, "source", |x| {
            let s = a::string(x)?;
            Ok(if s.equals_str("provider") {
                BudgetSource::Provider
            } else if s.equals_str("calibrated") {
                BudgetSource::Calibrated
            } else if s.equals_str("local") {
                BudgetSource::Local
            } else {
                return Err(a::invalid("Invalid budget source"));
            })
        })
        .ok()
        .flatten(),
        exact: a::opt_bool(v, "exact").ok().flatten(),
        reserved_output_tokens: a::opt_number(v, "reservedOutputTokens").ok().flatten(),
        estimator_error: a::opt_string(v, "estimatorError").ok().flatten(),
        breakdown: a::optional(v, "breakdown", |x| {
            if !a::str_field(x, "source")?.equals_str("local_estimate") {
                return Err(a::invalid("Invalid breakdown source"));
            }
            Ok(BudgetBreakdown {
                total: a::num_field(x, "total")?,
                system: a::num_field(x, "system")?,
                tools: a::num_field(x, "tools")?,
                messages: a::num_field(x, "messages")?,
                mcp: a::num_field(x, "mcp")?,
                memory: a::num_field(x, "memory")?,
            })
        })
        .ok()
        .flatten(),
    })
}
fn steer(v: &JsValue) -> PortResult<Vec<SteerMessage>> {
    a::array(v)?
        .iter()
        .map(|v| {
            Ok(SteerMessage {
                item_id: a::str_field(v, "itemId")?,
                message: a::message(a::field(v, "message")?)?,
                allowed_read_files: a::optional(v, "allowedReadFiles", a::strings)?,
                boundary: BoundarySnapshot::capture(v),
            })
        })
        .collect()
}
// Original default factory readRequiredToolDefinitions/readToolDescriptors.
// Normalized policy values coexist with the complete original entry boundary.
fn tool_descriptor(v: &JsValue) -> PortResult<ToolDescriptor> {
    let name = v
        .as_object()
        .and_then(|v| v.get("name"))
        .and_then(JsValue::as_string)
        .filter(|s| !s.is_empty())
        .cloned()
        .ok_or_else(|| {
            a::invalid(
                "Host tool catalog response contains an invalid or duplicate tool descriptor.",
            )
        })?;
    let nonempty = |v: Option<&JsValue>| {
        v.and_then(JsValue::as_string)
            .filter(|s| !s.is_empty())
            .cloned()
    };
    let description = nonempty(v.get("description")).unwrap_or_else(|| "Host capability".into());
    let kind = v
        .get("kind")
        .and_then(JsValue::as_string)
        .filter(|s| {
            [
                "filesystem",
                "shell",
                "network",
                "mcp",
                "session",
                "agent",
                "structured_output",
                "custom",
            ]
            .iter()
            .any(|kind| s.equals_str(kind))
        })
        .cloned()
        .unwrap_or_else(|| "custom".into());
    let requires_user_interaction =
        v.get("requiresUserInteraction").and_then(JsValue::as_bool) == Some(true);
    let mut capabilities = Vec::new();
    if let Some(JsValue::Array(items)) = v.get("requiredRuntimeCapabilities") {
        for value in items {
            if let Some(value) = value.as_string()
                && [
                    "always_on_run_context",
                    "plan_workflow",
                    "subagent_fork",
                    "user_interaction",
                ]
                .iter()
                .any(|c| value.equals_str(c))
                && !capabilities.contains(value)
            {
                capabilities.push(value.clone());
            }
        }
    }
    if kind.equals_str("agent") && !capabilities.contains(&JsString::from("subagent_fork")) {
        capabilities.push("subagent_fork".into());
    }
    if requires_user_interaction && !capabilities.contains(&JsString::from("user_interaction")) {
        capabilities.push("user_interaction".into());
    }
    let schema = nullish(v.get("inputSchema"))
        .or_else(|| v.get("input_schema"))
        .and_then(JsValue::as_object)
        .filter(|o| {
            o.get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("object"))
        })
        .cloned()
        .unwrap_or_else(|| obj([("type", JsValue::from("object"))]));
    Ok(ToolDescriptor {
        name,
        description,
        kind,
        input_schema: schema,
        read_only: v.get("readOnly").and_then(JsValue::as_bool) == Some(true),
        concurrency_safe: v.get("concurrencySafe").and_then(JsValue::as_bool) == Some(true),
        requires_user_interaction,
        required_runtime_capabilities: Some(capabilities),
    })
}
fn live_catalog(v: &JsValue) -> PortResult<Vec<Captured<ToolDescriptor>>> {
    let entries = match v {
        JsValue::Array(items) => items.as_slice(),
        _ => match v.get("available") {
            Some(JsValue::Array(items)) => items.as_slice(),
            _ => {
                return Err(a::invalid(
                    "Host tool catalog response must contain a tools array.",
                ));
            }
        },
    };
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let descriptor = tool_descriptor(entry)?;
        if !seen.insert(descriptor.name.clone()) {
            return Err(a::invalid(
                "Host tool catalog response contains an invalid or duplicate tool descriptor.",
            ));
        }
        result.push(capture(entry, descriptor));
    }
    Ok(result)
}
fn permission(v: &JsValue) -> PortResult<PermissionDecision> {
    let reason = a::field(v, "reason")?;
    if !matches!(reason, JsValue::Object(_) | JsValue::Array(_)) {
        return Err(a::invalid("Permission reason must be object-like"));
    }
    let reason = BoundarySnapshot::capture(reason);
    Ok(
        match label(v, "type", &["allow", "deny", "ask", "cancel"])? {
            0 => PermissionDecision::Allow {
                reason,
                updated_input: match v.get("updatedInput") {
                    None | Some(JsValue::Undefined) => None,
                    Some(x) => Some(x.clone()),
                },
            },
            1 => PermissionDecision::Deny {
                reason,
                message: a::str_field(v, "message")?,
            },
            2 => {
                let request = a::field(v, "request")?;
                if !matches!(request, JsValue::Object(_) | JsValue::Array(_)) {
                    return Err(a::invalid("Permission ask must contain request object"));
                }
                PermissionDecision::Ask {
                    reason,
                    request: BoundarySnapshot::capture(request),
                }
            }
            _ => PermissionDecision::Cancel {
                reason,
                message: a::str_field(v, "message")?,
            },
        },
    )
}
fn diagnostic(v: &JsValue) -> PortResult<ContextDiagnostic> {
    Ok(ContextDiagnostic {
        code: a::str_field(v, "code")?,
        severity: match label(v, "severity", &["info", "warning", "error", "fatal"])? {
            0 => DiagnosticSeverity::Info,
            1 => DiagnosticSeverity::Warning,
            2 => DiagnosticSeverity::Error,
            _ => DiagnosticSeverity::Fatal,
        },
        message: a::str_field(v, "message")?,
        path: a::opt_string(v, "path")?,
    })
}
fn diagnostics(v: &JsValue) -> PortResult<Vec<ContextDiagnostic>> {
    a::array(v)?.iter().map(diagnostic).collect()
}
fn prepared_context(v: &JsValue) -> PortResult<PreparedContext> {
    Ok(PreparedContext {
        messages: a::messages(a::field(v, "messages")?)?,
        system_prompt: a::opt_string(v, "systemPrompt")?,
        system_prompt_parts: a::strings(a::field(v, "systemPromptParts")?)?,
        tools: a::array(a::field(v, "tools")?)?
            .iter()
            .map(a::tool_schema)
            .collect::<PortResult<_>>()?,
        diagnostics: diagnostics(a::field(v, "diagnostics")?)?,
        boundaries: a::array(a::field(v, "boundaries")?)?
            .iter()
            .map(|x| {
                Ok(ContextBoundary {
                    kind: match label(x, "type", &["compact", "microcompact", "snip"])? {
                        0 => ContextBoundaryKind::Compact,
                        1 => ContextBoundaryKind::Microcompact,
                        _ => ContextBoundaryKind::Snip,
                    },
                    retained_messages: a::num_field(x, "retainedMessages")?,
                    metadata: a::nullable(x, "metadata", a::metadata)?,
                })
            })
            .collect::<PortResult<_>>()?,
        metadata: a::nullable(v, "metadata", a::metadata)?,
        materialization: a::optional(v, "materialization", |x| {
            Ok(ContextMaterialization {
                step_id: a::opt_number(x, "stepId")?,
                prompt_generation: a::opt_number(x, "promptGeneration")?,
                runtime_contexts: a::array(a::field(x, "runtimeContexts")?)?
                    .iter()
                    .map(|x| {
                        Ok(RuntimeContextSection {
                            name: a::str_field(x, "name")?,
                            text: a::str_field(x, "text")?,
                        })
                    })
                    .collect::<PortResult<_>>()?,
                runtime_context_messages: a::optional(x, "runtimeContextMessages", a::messages)?,
                instruction_layers: a::optional(x, "instructionLayers", |x| {
                    a::array(x)?
                        .iter()
                        .map(|x| {
                            Ok(InstructionLayer {
                                scope: a::str_field(x, "scope")?,
                                path: a::str_field(x, "path")?,
                                content: a::str_field(x, "content")?,
                            })
                        })
                        .collect()
                })?,
            })
        })?,
        cache_breakpoints: a::optional(v, "cacheBreakpoints", a::numbers)?,
        cache_plan: a::optional(v, "cachePlan", a::cache_plan)?,
    })
}
fn recovery(v: &JsValue) -> PortResult<RecoveryDecision> {
    let reason = a::str_field(v, "reason")?;
    Ok(
        match label(
            v,
            "type",
            &[
                "truncate_head_and_retry",
                "adjust_output_and_retry",
                "compact_and_retry",
                "strip_images_and_retry",
                "give_up",
            ],
        )? {
            0 => RecoveryDecision::TruncateHeadAndRetry {
                keep_ratio: a::num_field(v, "keepRatio")?,
                reason,
            },
            1 => RecoveryDecision::AdjustOutputAndRetry {
                max_output_tokens: a::num_field(v, "maxOutputTokens")?,
                reason,
                scope: a::optional(v, "scope", |x| {
                    let s = a::string(x)?;
                    if s.equals_str("hard_cap") {
                        Ok(OutputLimitScope::HardCap)
                    } else if s.equals_str("attempt") {
                        Ok(OutputLimitScope::Attempt)
                    } else {
                        Err(a::invalid("Invalid output scope"))
                    }
                })?,
            },
            2 => RecoveryDecision::CompactAndRetry {
                max_context_tokens: a::opt_number(v, "maxContextTokens")?,
                max_output_tokens: a::opt_number(v, "maxOutputTokens")?,
                reason,
            },
            3 => RecoveryDecision::StripImagesAndRetry { reason },
            _ => RecoveryDecision::GiveUp { reason },
        },
    )
}
fn compact(v: &JsValue) -> PortResult<AutoCompactResult> {
    let snap = a::field(v, "snapshot")?;
    let snapshot = capture(snap, budget(snap)?);
    Ok(if label(v, "type", &["skipped", "compacted"])? == 0 {
        AutoCompactResult::Skipped { snapshot }
    } else {
        AutoCompactResult::Compacted {
            messages: a::messages(a::field(v, "messages")?)?,
            tier: match label(v, "tier", &["micro", "snip", "full", "emergency"])? {
                0 => CompactionTier::Micro,
                1 => CompactionTier::Snip,
                2 => CompactionTier::Full,
                _ => CompactionTier::Emergency,
            },
            snapshot,
            result: a::optional(v, "result", |x| Ok(BoundarySnapshot::capture(x)))?,
            overflow_after_emergency: v
                .get("error")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("context_overflow_after_emergency_compaction")),
        }
    })
}
fn snapshots(v: &JsValue) -> PortResult<Vec<BoundarySnapshot>> {
    Ok(a::array(v)?.iter().map(BoundarySnapshot::capture).collect())
}
fn normalized_hook_effect(v: &JsValue) -> PortResult<HookEffect> {
    Ok(
        match label(
            v,
            "type",
            &[
                "additional_context",
                "system_message",
                "block",
                "permission_decision",
                "updated_tool_input",
                "updated_mcp_tool_output",
                "permission_request_result",
                "initial_user_message",
                "watch_paths",
                "worktree_path",
                "retry_permission_denied",
            ],
        )? {
            0 => HookEffect::AdditionalContext {
                content: a::str_field(v, "content")?,
                source: a::str_field(v, "source")?,
            },
            1 => HookEffect::SystemMessage {
                content: a::str_field(v, "content")?,
            },
            2 => HookEffect::Block {
                reason: a::str_field(v, "reason")?,
                stop_reason: a::opt_string(v, "stopReason")?,
            },
            3 => HookEffect::PermissionDecision {
                behavior: a::str_field(v, "behavior")?,
                reason: a::opt_string(v, "reason")?,
            },
            4 => HookEffect::UpdatedToolInput {
                input: a::metadata(a::field(v, "input")?)?,
            },
            5 => HookEffect::UpdatedMcpToolOutput {
                output: a::field(v, "output")?.clone(),
            },
            6 => HookEffect::PermissionRequestResult {
                result: BoundarySnapshot::capture(a::field(v, "result")?),
            },
            7 => HookEffect::InitialUserMessage {
                message: a::str_field(v, "message")?,
            },
            8 => HookEffect::WatchPaths {
                paths: a::strings(a::field(v, "paths")?)?,
            },
            9 => HookEffect::WorktreePath {
                path: a::str_field(v, "path")?,
            },
            _ => HookEffect::RetryPermissionDenied,
        },
    )
}
fn hook_effect(v: &JsValue) -> PortResult<HookEffect> {
    Ok(
        normalized_hook_effect(v).unwrap_or_else(|_| HookEffect::BoundaryOnly {
            boundary: BoundarySnapshot::capture(v),
        }),
    )
}
fn lifecycle(v: &JsValue) -> PortResult<LifecycleResult> {
    Ok(LifecycleResult {
        effects: a::array(a::field(v, "effects")?)?
            .iter()
            .map(hook_effect)
            .collect::<PortResult<_>>()?,
        messages: snapshots(a::field(v, "messages")?)?,
        events: snapshots(a::field(v, "events")?)?,
        blocking_errors: snapshots(a::field(v, "blockingErrors")?)?,
        non_blocking_errors: snapshots(a::field(v, "nonBlockingErrors")?)?,
        pending_async_hooks: a::optional(v, "pendingAsyncHooks", snapshots)?,
    })
}
fn todo(v: &JsValue) -> PortResult<TodoItem> {
    Ok(TodoItem {
        id: a::opt_string(v, "id")?,
        content: a::str_field(v, "content")?,
        status: match label(
            v,
            "status",
            &["pending", "in_progress", "completed", "cancelled"],
        )? {
            0 => TodoStatus::Pending,
            1 => TodoStatus::InProgress,
            2 => TodoStatus::Completed,
            _ => TodoStatus::Cancelled,
        },
        priority: a::opt_string(v, "priority")?,
        boundary: BoundarySnapshot::capture(v),
    })
}
fn todos(v: &JsValue) -> PortResult<Vec<TodoItem>> {
    a::array(v)?.iter().map(todo).collect()
}
fn todo_mode(v: &JsValue) -> PortResult<TodoWriteMode> {
    Ok(if a::string(v)?.equals_str("markdown") {
        TodoWriteMode::Markdown
    } else if a::string(v)?.equals_str("structured") {
        TodoWriteMode::Structured
    } else {
        return Err(a::invalid("Invalid todo mode"));
    })
}
fn todo_last(v: &JsValue) -> PortResult<TodoLastWrite> {
    Ok(TodoLastWrite {
        mode: todo_mode(a::field(v, "mode")?)?,
        merge: a::bool_field(v, "merge")?,
        reason: a::opt_string(v, "reason")?,
        added_count: a::num_field(v, "addedCount")?,
        removed_count: a::num_field(v, "removedCount")?,
        changed_count: a::num_field(v, "changedCount")?,
        deleted_open_item_count: a::num_field(v, "deletedOpenItemCount")?,
        large_rewrite: a::bool_field(v, "largeRewrite")?,
        all_completed: a::bool_field(v, "allCompleted")?,
    })
}
fn todo_diagnostics(v: &JsValue) -> PortResult<TodoDiagnostics> {
    Ok(TodoDiagnostics {
        write_count: a::num_field(v, "writeCount")?,
        todo_count: a::num_field(v, "todoCount")?,
        active_count: a::num_field(v, "activeCount")?,
        completed_count: a::num_field(v, "completedCount")?,
        cancelled_count: a::num_field(v, "cancelledCount")?,
        large_rewrite_count: a::num_field(v, "largeRewriteCount")?,
        deleted_open_item_count: a::num_field(v, "deletedOpenItemCount")?,
        completed_without_active_count: a::num_field(v, "completedWithoutActiveCount")?,
        last_write: a::optional(v, "lastWrite", todo_last)?,
    })
}
fn todo_diagnostic_value(v: &TodoDiagnostics) -> JsValue {
    let mut o = obj([
        ("writeCount", n(v.write_count)),
        ("todoCount", n(v.todo_count)),
        ("activeCount", n(v.active_count)),
        ("completedCount", n(v.completed_count)),
        ("cancelledCount", n(v.cancelled_count)),
        ("largeRewriteCount", n(v.large_rewrite_count)),
        ("deletedOpenItemCount", n(v.deleted_open_item_count)),
        (
            "completedWithoutActiveCount",
            n(v.completed_without_active_count),
        ),
    ]);
    if let Some(last) = &v.last_write {
        let mut l = obj([
            (
                "mode",
                JsValue::from(match last.mode {
                    TodoWriteMode::Markdown => "markdown",
                    TodoWriteMode::Structured => "structured",
                }),
            ),
            ("merge", b(last.merge)),
        ]);
        opt(&mut l, "reason", last.reason.as_ref(), s);
        for (k, v) in [
            ("addedCount", last.added_count),
            ("removedCount", last.removed_count),
            ("changedCount", last.changed_count),
            ("deletedOpenItemCount", last.deleted_open_item_count),
        ] {
            put(&mut l, k, n(v));
        }
        put(&mut l, "largeRewrite", b(last.large_rewrite));
        put(&mut l, "allCompleted", b(last.all_completed));
        put(&mut o, "lastWrite", JsValue::Object(l));
    }
    JsValue::Object(o)
}
fn js_trim(v: &JsString) -> JsString {
    fn ws(v: u16) -> bool {
        matches!(v,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
    }
    let units = v.units();
    let start = units.iter().position(|v| !ws(*v)).unwrap_or(units.len());
    let end = units.iter().rposition(|v| !ws(*v)).map_or(start, |i| i + 1);
    JsString::from_utf16(units[start..end].to_vec())
}
fn active(t: &TodoItem) -> bool {
    matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress)
}
fn plan_todo_checkpoint_shape_error(value: &JsValue) -> Option<&'static str> {
    if !matches!(value, JsValue::Object(_)) {
        return Some("plan/todo checkpoint must be an object.");
    }
    let valid_count = value
        .get("toolCallsSinceLastTodoWrite")
        .and_then(JsValue::as_number)
        .is_some_and(|n| {
            n.is_finite() && (0.0..=9007199254740991.0).contains(&n) && n.fract() == 0.0
        });
    if !matches!(value.get("requiresInitialization"), Some(JsValue::Bool(_)))
        || !valid_count
        || ["todos", "activeTodos", "todoHistory"]
            .iter()
            .any(|key| !matches!(value.get(key), Some(JsValue::Array(_))))
        || !matches!(
            value.get("todoDiagnostics"),
            Some(JsValue::Object(_) | JsValue::Array(_))
        )
    {
        return Some("plan/todo checkpoint is invalid.");
    }
    None
}
// Original exported parsePlanTodoSnapshot admission and replay exception boundary.
// JSON-compatible spread/property reads in applyTodoWrite accept lax item/merge,
// createdAt/markdown/diagnostic values. Only actual trim calls can fail here.
fn source_plan_todo_admission(v: &JsValue) -> PortResult<()> {
    a::object(v)?;
    a::bool_field(v, "requiresInitialization")?;
    let count = a::num_field(v, "toolCallsSinceLastTodoWrite")?;
    if !count.is_finite() || !(0.0..=9007199254740991.0).contains(&count) || count.fract() != 0.0 {
        return Err(a::invalid("Invalid plan/todo call count"));
    }
    a::array(a::field(v, "todos")?)?;
    a::array(a::field(v, "activeTodos")?)?;
    let diagnostics = a::field(v, "todoDiagnostics")?;
    if !matches!(diagnostics, JsValue::Object(_) | JsValue::Array(_)) {
        return Err(a::invalid(
            "Plan/todo diagnostics must be nonnull object-like",
        ));
    }
    if let Some(plan) = v.get("approvedPlan") {
        let truthy = match plan {
            JsValue::Undefined | JsValue::Null => false,
            JsValue::Bool(v) => *v,
            JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
            JsValue::String(v) => !v.is_empty(),
            _ => true,
        };
        if truthy {
            a::string(plan)?;
        }
    }
    for history in a::array(a::field(v, "todoHistory")?)? {
        if !matches!(history, JsValue::Object(_) | JsValue::Array(_))
            || !matches!(history.get("todos"), Some(JsValue::Array(_)))
            || !history
                .get("mode")
                .and_then(JsValue::as_string)
                .is_some_and(|mode| mode.equals_str("markdown") || mode.equals_str("structured"))
        {
            return Err(PortError::KnownConsumerException {
                message: "plan/todo checkpoint history is invalid.".into(),
                boundary: BoundarySnapshot::capture(history),
            });
        }
        // entry.reason?.trim() skips null/Undefined, not false/0/nonstring.
        if let Some(reason) = nullish(history.get("reason")) {
            a::string(reason)?;
        }
    }
    Ok(())
}
fn plan_todo(v: &JsValue) -> PortResult<PlanTodoSnapshot> {
    let requires_initialization = a::bool_field(v, "requiresInitialization")?;
    let count = a::num_field(v, "toolCallsSinceLastTodoWrite")?;
    if !count.is_finite() || !(0.0..=9007199254740991.0).contains(&count) || count.fract() != 0.0 {
        return Err(a::invalid("Invalid plan/todo call count"));
    }
    a::array(a::field(v, "todos")?)?;
    a::array(a::field(v, "activeTodos")?)?;
    let approved_plan = a::opt_string(v, "approvedPlan")?
        .map(|s| js_trim(&s))
        .filter(|s| !s.is_empty());
    let mut current: Vec<TodoItem> = Vec::new();
    let mut history = Vec::new();
    let mut last_markdown = None;
    let mut large_rewrites = 0.0;
    let mut deleted_open = 0.0;
    let mut completed_without_active = 0.0;
    for (index, item) in a::array(a::field(v, "todoHistory")?)?.iter().enumerate() {
        let next = todos(a::field(item, "todos")?)?;
        let mode = todo_mode(a::field(item, "mode")?)?;
        let merge = a::bool_field(item, "merge")?;
        let reason = a::nullable(item, "reason", a::string)?
            .map(|s| js_trim(&s))
            .filter(|s| !s.is_empty());
        let markdown = a::opt_string(item, "markdown")?;
        let key = |t: &TodoItem| t.id.clone().unwrap_or_else(|| t.content.clone());
        let previous: std::collections::HashMap<_, _> =
            current.iter().map(|t| (key(t), t)).collect();
        let next_map: std::collections::HashMap<_, _> = next.iter().map(|t| (key(t), t)).collect();
        let removed: Vec<_> = current
            .iter()
            .filter(|t| !next_map.contains_key(&key(t)))
            .collect();
        let added = next
            .iter()
            .filter(|t| !previous.contains_key(&key(t)))
            .count();
        let changed = next
            .iter()
            .filter(|t| {
                previous.get(&key(t)).is_some_and(|old| {
                    old.content != t.content || old.status != t.status || old.priority != t.priority
                })
            })
            .count();
        let preserved = next
            .iter()
            .filter(|t| previous.contains_key(&key(t)))
            .count();
        let large =
            !current.is_empty() && !next.is_empty() && preserved < current.len().div_ceil(2);
        let del = removed.iter().filter(|t| active(t)).count();
        let active_count = next.iter().filter(|t| active(t)).count();
        let all_completed = !next.is_empty()
            && active_count == 0
            && next
                .iter()
                .all(|t| matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled));
        large_rewrites += if large { 1.0 } else { 0.0 };
        deleted_open += del as f64;
        completed_without_active += if all_completed && current.iter().any(active) {
            1.0
        } else {
            0.0
        };
        let diag = TodoDiagnostics {
            write_count: (index + 1) as f64,
            todo_count: next.len() as f64,
            active_count: active_count as f64,
            completed_count: next
                .iter()
                .filter(|t| t.status == TodoStatus::Completed)
                .count() as f64,
            cancelled_count: next
                .iter()
                .filter(|t| t.status == TodoStatus::Cancelled)
                .count() as f64,
            large_rewrite_count: large_rewrites,
            deleted_open_item_count: deleted_open,
            completed_without_active_count: completed_without_active,
            last_write: Some(TodoLastWrite {
                mode,
                merge,
                reason: reason.clone(),
                added_count: added as f64,
                removed_count: removed.len() as f64,
                changed_count: changed as f64,
                deleted_open_item_count: del as f64,
                large_rewrite: large,
                all_completed,
            }),
        };
        // History replay is the original consumer's projection; the complete incoming
        // snapshot remains separately captured by HostReply::PlanTodo.
        let created_at = a::str_field(item, "createdAt")?;
        let diag_value = todo_diagnostic_value(&diag);
        let mut history_value = obj([
            ("createdAt", s(&created_at)),
            (
                "mode",
                JsValue::from(match mode {
                    TodoWriteMode::Markdown => "markdown",
                    TodoWriteMode::Structured => "structured",
                }),
            ),
            ("merge", b(merge)),
        ]);
        opt(&mut history_value, "reason", reason.as_ref(), s);
        opt(&mut history_value, "markdown", markdown.as_ref(), s);
        put(
            &mut history_value,
            "todos",
            JsValue::Array(next.iter().map(|t| boundary(&t.boundary)).collect()),
        );
        put(&mut history_value, "diagnostics", diag_value.clone());
        history.push(capture(
            &JsValue::Object(history_value),
            TodoWriteHistory {
                created_at,
                mode,
                merge,
                reason,
                markdown: markdown.clone(),
                todos: next.clone(),
                diagnostics: capture(&diag_value, diag),
            },
        ));
        current = next;
        last_markdown = markdown;
    }
    if let Some(markdown) = a::opt_string(v, "lastMarkdown")? {
        last_markdown = Some(markdown)
    }
    let d = a::field(v, "todoDiagnostics")?;
    Ok(PlanTodoSnapshot {
        approved_plan,
        requires_initialization,
        tool_calls_since_last_todo_write: count,
        last_markdown,
        active_todos: current.iter().filter(|t| active(t)).cloned().collect(),
        todos: current,
        todo_history: history,
        todo_diagnostics: capture(d, todo_diagnostics(d)?),
    })
}
fn js_string(v: &JsValue) -> PortResult<JsString> {
    let mut result = Vec::new();
    let mut pending = vec![Some((v, false))];
    while let Some(task) = pending.pop() {
        let Some((value, in_array)) = task else {
            result.push(44);
            continue;
        };
        match value {
            JsValue::Undefined if in_array => {}
            JsValue::Null if in_array => {}
            JsValue::Undefined => result.extend("undefined".encode_utf16()),
            JsValue::Null => result.extend("null".encode_utf16()),
            JsValue::String(s) => result.extend_from_slice(s.units()),
            JsValue::Bool(b) => result.extend(if *b { "true" } else { "false" }.encode_utf16()),
            JsValue::Number(n) => {
                let text = if n.is_nan() {
                    "NaN".into()
                } else if *n == f64::INFINITY {
                    "Infinity".into()
                } else if *n == f64::NEG_INFINITY {
                    "-Infinity".into()
                } else {
                    pilotdeck_protocol::stringify_json(value)
                        .map_err(|_| a::invalid("Number stringify failed"))?
                        .ok_or_else(|| a::invalid("Number stringify was undefined"))?
                };
                result.extend(text.encode_utf16());
            }
            JsValue::Object(o) => {
                if o.contains_key("toString") {
                    return Err(a::invalid(
                        "JS object primitive conversion fails with own non-callable toString",
                    ));
                }
                result.extend("[object Object]".encode_utf16());
            }
            JsValue::Array(values) => {
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(Some((value, true)));
                    if index > 0 {
                        pending.push(None);
                    }
                }
            }
        }
    }
    Ok(JsString::from_utf16(result))
}

fn nullish(v: Option<&JsValue>) -> Option<&JsValue> {
    v.filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
}
fn failure(response: &JsValue, call: &HostCall) -> PortResult<PortError> {
    if call.method().0 == "model"
        && !matches!(call, HostCall::ModelMetadata { .. })
        && let Some(canonical) = response.get("error").and_then(|e| e.get("canonical"))
        && canonical.get("code").and_then(JsValue::as_string).is_some()
        && canonical
            .get("message")
            .and_then(JsValue::as_string)
            .is_some()
    {
        return Ok(match a::model_error(canonical) {
            Ok(view) => PortError::Provider(Box::new(capture(canonical, view))),
            Err(_) => PortError::ProviderBoundaryOnly {
                code: a::str_field(canonical, "code")?,
                message: a::str_field(canonical, "message")?,
                boundary: BoundarySnapshot::capture(canonical),
            },
        });
    }
    let fallback = match call {
        HostCall::ModelMetadata { .. } => "Host model metadata lookup failed.".to_owned(),
        HostCall::ModelPrepare { .. } => "Model prepare failed".to_owned(),
        HostCall::ModelMaterialize { .. } => "Model request materialization failed".to_owned(),
        HostCall::ModelPull { .. } => "Model stream failed".to_owned(),
        HostCall::ModelEagerStream { .. } | HostCall::ModelClose { .. } => {
            "Model module failed".to_owned()
        }
        HostCall::DecidePermission { .. } => "Permission module failed.".to_owned(),
        HostCall::PlanTodo(_) => "Plan/todo module failed.".to_owned(),
        HostCall::DispatchLifecycle { .. } => "Lifecycle module failed.".to_owned(),
        HostCall::EmitEvent { .. } => "Agent event module failed.".to_owned(),
        HostCall::ListTools => "Host tool catalog refresh failed.".to_owned(),
        HostCall::ExecuteSingle { .. } | HostCall::ExecuteBatch { .. } => {
            "Capability module failed.".to_owned()
        }
        _ => {
            let (m, op) = call.method();
            format!(
                "{} module {op} failed.",
                match m {
                    "budget" => "Budget",
                    "turn" => "Turn",
                    _ => "Context",
                }
            )
        }
    };
    let error_message = response
        .get("error")
        .and_then(|e| e.get("message"))
        .filter(|v| {
            !matches!(call, HostCall::ModelMetadata { .. }) || matches!(v, JsValue::String(_))
        });
    let message = if let Some(v) = nullish(error_message).or_else(|| nullish(response.get("code")))
    {
        js_string(v)?
    } else {
        fallback.into()
    };
    let code = nullish(response.get("code"))
        .map(js_string)
        .transpose()?
        .unwrap_or_default();
    Ok(PortError::HostFailure {
        code,
        message,
        boundary: BoundarySnapshot::capture(response),
    })
}
fn known_event_kind(v: &JsValue) -> bool {
    v.get("type").and_then(JsValue::as_string).is_some_and(|s| {
        [
            "request_started",
            "message_start",
            "text_delta",
            "thinking_delta",
            "tool_call_start",
            "tool_call_delta",
            "tool_call_end",
            "message_end",
            "usage",
            "error",
        ]
        .iter()
        .any(|t| s.equals_str(t))
    })
}
fn coerced_delta(v: &JsValue) -> PortResult<StreamEvent> {
    let original = a::object(v)?;
    let mut reducer = original.clone();
    let undefined = JsValue::Undefined;
    let text = match js_string(v.get("text").unwrap_or(&undefined)) {
        Ok(text) => text,
        // JSON wire objects cannot contain a callable toString. JS throws at
        // this exact buffer append after the original live event is forwarded.
        Err(PortError::InvalidResponse { message, .. })
            if message.equals_str(
                "JS object primitive conversion fails with own non-callable toString",
            ) =>
        {
            return Ok(StreamEvent::DeferredSourceException {
                boundary: BoundarySnapshot::capture(v),
                message: "Cannot convert object to primitive value".into(),
            });
        }
        Err(error) => return Err(error),
    };
    reducer.insert("text".into(), JsValue::String(text));
    if v.get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str("thinking_delta"))
    {
        if let Some(value) = v.get("reasoningContent")
            && !value.is_undefined()
        {
            reducer.insert(
                "reasoningContent".into(),
                JsValue::String(js_string(value)?),
            );
        }
        match v.get("signature") {
            None | Some(JsValue::Undefined | JsValue::String(_)) => {}
            // Numeric/boolean signatures have no length property in original JS.
            Some(JsValue::Number(_) | JsValue::Bool(_)) => {
                reducer.insert("signature".into(), JsValue::Undefined);
            }
            Some(JsValue::Array(items)) if items.is_empty() => {
                reducer.insert("signature".into(), JsValue::Undefined);
            }
            _ => {
                return Err(a::invalid(
                    "Pending original non-string signature effect/exception; never silently ignore it",
                ));
            }
        }
    }
    let reducer = a::event(&JsValue::Object(reducer))?;
    Ok(StreamEvent::CoercedDelta {
        boundary: BoundarySnapshot::capture(v),
        reducer: Box::new(reducer),
    })
}
fn stream_events(v: &JsValue) -> PortResult<Vec<StreamEvent>> {
    a::array(v)?
        .iter()
        .map(|v| {
            if !known_event_kind(v) {
                return Ok(StreamEvent::BoundaryOnly(BoundarySnapshot::capture(v)));
            }
            let kind = v
                .get("type")
                .and_then(JsValue::as_string)
                .expect("known event has string type");
            if ["message_start", "tool_call_start", "tool_call_delta"]
                .iter()
                .any(|tag| kind.equals_str(tag))
            {
                return Ok(StreamEvent::BoundaryOnly(BoundarySnapshot::capture(v)));
            }
            if kind.equals_str("text_delta") || kind.equals_str("thinking_delta") {
                return match a::event(v) {
                    Ok(event) => Ok(StreamEvent::Known(Box::new(event))),
                    // The original host port admits this already successful
                    // event boundary without checking normalized model fields.
                    // Optional legacy projection must not convert a consumer
                    // coercion/shape limitation into transport uncertainty.
                    Err(_) => Ok(coerced_delta(v).unwrap_or_else(|_| {
                        StreamEvent::BoundaryOnly(BoundarySnapshot::capture(v))
                    })),
                };
            }
            match a::event(v) {
                Ok(e) => Ok(StreamEvent::Known(Box::new(e))),
                Err(_error)
                    if v.get("type")
                        .and_then(JsValue::as_string)
                        .is_some_and(|s| s.equals_str("error")) =>
                {
                    Ok(StreamEvent::ErrorBoundaryOnly {
                        boundary: BoundarySnapshot::capture(v),
                        error: BoundarySnapshot::capture(
                            v.get("error").unwrap_or(&JsValue::Undefined),
                        ),
                    })
                }
                // SourceModelReducer owns open known fields and their actual
                // ordered exceptions. A valid reply is not AcceptedUnknown
                // merely because the closed normalized model cannot project it.
                Err(_) => Ok(StreamEvent::BoundaryOnly(BoundarySnapshot::capture(v))),
            }
        })
        .collect()
}
fn positive_safe(value: Option<&JsValue>) -> Option<f64> {
    value
        .and_then(JsValue::as_number)
        .filter(|n| n.is_finite() && *n > 0.0 && n.fract() == 0.0 && *n <= 9007199254740991.0)
}
fn tool_result_unknown(v: &JsValue) -> bool {
    let error = v.get("error");
    let details = error.and_then(|e| e.get("details"));
    details
        .and_then(|d| d.get("outcome"))
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str("result_unknown"))
}
fn cancelled_tool_result(v: &JsValue, failed: bool) -> PortResult<bool> {
    if !failed
        && !v
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("error"))
    {
        return Ok(false);
    }
    let error = v.get("error");
    if !failed && !error.is_some_and(|e| matches!(e, JsValue::Object(_) | JsValue::Array(_))) {
        return Ok(false);
    }
    let code_value = if failed {
        nullish(v.get("code")).or_else(|| error.and_then(|e| nullish(e.get("code"))))
    } else {
        error.and_then(|e| nullish(e.get("code")))
    };
    let code = code_value.map(js_string).transpose()?.unwrap_or_default();
    let message = error
        .and_then(|e| nullish(e.get("message")))
        .map(js_string)
        .transpose()?
        .unwrap_or_default();
    let known = ["CANCELLED", "ABORTED", "TOOL_ABORTED"].iter().any(|word| {
        code.units()
            .iter()
            .copied()
            .map(|c| if (97..=122).contains(&c) { c - 32 } else { c })
            .eq(word.encode_utf16())
    });
    let mut text = Vec::new();
    if failed {
        text.extend_from_slice(code.units());
        text.push(32);
    }
    text.extend_from_slice(message.units());
    fn word(c: u16) -> bool {
        matches!(c,48..=57|65..=90|97..=122|95)
    }
    let text_match = (0..text.len()).any(|start| {
        if start > 0 && word(text[start - 1]) {
            return false;
        }
        ["cancel", "cancelled", "cancellation"]
            .iter()
            .any(|suffix| {
                let expected: Vec<_> = suffix.encode_utf16().collect();
                let end = start + expected.len();
                end <= text.len()
                    && text[start..end]
                        .iter()
                        .copied()
                        .map(|c| if (65..=90).contains(&c) { c + 32 } else { c })
                        .eq(expected)
                    && (end == text.len() || !word(text[end]))
            })
    });
    Ok(known || text_match)
}
fn reply(response: &JsValue, call: &HostCall) -> PortResult<HostReply> {
    let undefined = JsValue::Undefined;
    let p = response.get("payload").unwrap_or(&undefined);
    Ok(match call {
        HostCall::ModelMetadata { route } => {
            let v = p.get("metadata").unwrap_or(&undefined);
            let view = model_metadata(v)?;
            if &view.route != route {
                return Err(a::invalid(
                    "Host model metadata response does not match the requested route.",
                ));
            }
            HostReply::ModelMetadata(capture(v, view))
        }
        HostCall::ModelPrepare { handle, .. } => {
            let v = a::field(p, "prepared")?;
            let route = Route {
                provider: a::str_field(v, "provider")?,
                model: a::str_field(v, "model")?,
            };
            let request = a::bound_request(a::field(v, "request")?, &route)?;
            let metadata = a::optional(p, "metadata", |v| {
                let view = model_metadata(v)?;
                if view.route != route {
                    return Err(a::invalid(
                        "Host model metadata response does not match the prepared route.",
                    ));
                }
                Ok(capture(v, view))
            })?;
            HostReply::ModelPrepared(Box::new(PreparedInvocation {
                handle: handle.clone(),
                request,
                route,
                max_context_tokens: positive_safe(v.get("maxContextTokens")),
                max_output_tokens: positive_safe(v.get("maxOutputTokens")),
                metadata,
                boundary: BoundarySnapshot::capture(v),
            }))
        }
        HostCall::ModelMaterialize { request, .. } => HostReply::ModelMaterialized(Box::new(
            a::bound_request(a::field(p, "request")?, &request.route)?,
        )),
        HostCall::ModelPull { .. } => {
            let events = a::field(p, "events")?;
            a::array(events)?;
            let done = a::bool_field(p, "done")?;
            HostReply::ModelChunk(StreamChunk {
                events: stream_events(events)?,
                done,
            })
        }
        HostCall::ModelClose { .. } => HostReply::ModelClosed,
        HostCall::ModelEagerStream { .. } => {
            HostReply::ModelEagerEvents(stream_events(a::field(p, "events")?)?)
        }
        HostCall::BudgetEstimate { .. } => {
            HostReply::BudgetEstimate(nonnegative(a::field(p, "tokens")?)?)
        }
        HostCall::BudgetEvaluate { .. } => {
            let v = a::field(p, "snapshot")?;
            HostReply::BudgetSnapshot(capture(v, budget(v)?))
        }
        HostCall::BudgetCost { .. } | HostCall::BudgetCostSnapshot { .. } => {
            HostReply::BudgetCost(a::nullable(p, "costUsd", nonnegative)?)
        }
        HostCall::TurnDrain => HostReply::SteerMessages(steer(a::field(p, "messages")?)?),
        HostCall::TurnDrainOrClose => {
            let closed = a::bool_field(p, "closed")?;
            HostReply::SteerDrain(SteerDrain {
                messages: steer(a::field(p, "messages")?)?,
                closed,
            })
        }
        HostCall::TurnPersistCompaction { .. } => {
            HostReply::CompactionCommitted(CompactionCommit {
                response: BoundarySnapshot::capture(response),
            })
        }
        HostCall::ListTools => {
            HostReply::Tools(live_catalog(p.get("tools").unwrap_or(&JsValue::Undefined))?)
        }
        HostCall::ExecuteBatch { calls, .. } => {
            let results = a::array(a::field(p, "results")?)?;
            if results.len() != calls.len() {
                return Err(a::invalid(
                    "Capability batch requires one ordered result for each call",
                ));
            }
            for (index, (v, call)) in results.iter().zip(calls).enumerate() {
                a::object(v)?;
                a::field(v, "type")?;
                if a::str_field(v, "toolCallId")? != call.tool_call_id {
                    return Err(a::invalid(format!(
                        "Capability result {index} has wrong toolCallId"
                    )));
                }
            }
            let mut admitted = Vec::with_capacity(results.len());
            for v in results {
                match a::tool_result(v) {
                    Ok(result) => admitted.push(result),
                    Err(_) => {
                        return Ok(HostReply::ToolResultsBoundaryOnly(
                            results.iter().map(BoundarySnapshot::capture).collect(),
                        ));
                    }
                };
            }
            HostReply::ToolResults(admitted)
        }
        HostCall::ExecuteSingle { call, .. } => {
            a::object(p)?;
            a::field(p, "type")?;
            if let Some(id) = p.get("toolCallId")
                && a::string(id)? != call.tool_call_id
            {
                return Err(a::invalid("Single capability result has wrong toolCallId"));
            }
            match a::tool_result(p) {
                Ok(result) => HostReply::ToolResult(Box::new(result)),
                Err(_) => HostReply::ToolResultBoundaryOnly(BoundarySnapshot::capture(p)),
            }
        }
        HostCall::PlanTodo(_) => {
            let undefined_snapshot = JsValue::Undefined;
            let v = p.get("snapshot").unwrap_or(&undefined_snapshot);
            if let Some(message) = plan_todo_checkpoint_shape_error(v) {
                return Err(PortError::KnownConsumerException {
                    message: message.into(),
                    boundary: BoundarySnapshot::capture(v),
                });
            }
            source_plan_todo_admission(v)?;
            match plan_todo(v) {
                Ok(view) => HostReply::PlanTodo(capture(v, view)),
                Err(_) => HostReply::PlanTodoBoundaryOnly(BoundarySnapshot::capture(v)),
            }
        }
        HostCall::DecidePermission { .. } => {
            let v = a::field(p, "decision")?;
            HostReply::Permission(capture(v, permission(v)?))
        }
        HostCall::PrepareContext(_) => {
            let v = a::field(p, "result")?;
            match prepared_context(v) {
                Ok(view) => HostReply::Context(Box::new(capture(v, view))),
                Err(_) => HostReply::ContextBoundaryOnly(BoundarySnapshot::capture(v)),
            }
        }
        HostCall::ApplyToolResults(_) => {
            let v = a::field(p, "result")?;
            match (|| -> PortResult<_> {
                Ok(AppliedToolResults {
                    messages: a::messages(a::field(v, "messages")?)?,
                    appended_messages: a::optional(v, "appendedMessages", a::messages)?,
                    diagnostics: diagnostics(a::field(v, "diagnostics")?)?,
                })
            })() {
                Ok(view) => HostReply::ToolResultsApplied(capture(v, view)),
                Err(_) => HostReply::ToolResultsAppliedBoundaryOnly(BoundarySnapshot::capture(v)),
            }
        }
        HostCall::RecoverModelError(_) | HostCall::RecoverModelErrorBoundary(_) => {
            let v = a::field(p, "result")?;
            match recovery(v) {
                Ok(view) => HostReply::Recovery(capture(v, view)),
                Err(_) => HostReply::RecoveryBoundaryOnly(BoundarySnapshot::capture(v)),
            }
        }
        HostCall::CaptureTurn(_) => {
            a::field(p, "result")?;
            HostReply::CapturedTurn
        }
        HostCall::AutoCompact(_) => {
            let v = a::field(p, "result")?;
            match compact(v) {
                Ok(view) => HostReply::Compacted(Box::new(capture(v, view))),
                Err(_) => HostReply::CompactedBoundaryOnly(BoundarySnapshot::capture(v)),
            }
        }
        HostCall::DispatchLifecycle { .. } => {
            let v = a::field(p, "result")?;
            HostReply::Lifecycle(capture(v, lifecycle(v)?))
        }
        HostCall::EmitEvent { .. } => HostReply::EventEmitted,
    })
}
fn wire_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn module_failure_tool_result<E: HostExchange>(
    exchange: &E,
    response: &JsValue,
    tool: &pilotdeck_agent::ports::ToolInvocation,
) -> PortResult<pilotdeck_agent::ToolExecutionResult> {
    // Original hostToolPort.moduleFailureResult: a known failed response is a
    // result for each call; it does not enter AgentLoop's missing-result catch.
    let undefined = JsValue::Undefined;
    let raw_code = response.get("code").unwrap_or(&undefined);
    let code = raw_code
        .as_string()
        .filter(|s| {
            [
                "tool_not_found",
                "tool_unavailable",
                "invalid_tool_input",
                "permission_denied",
                "permission_cancelled",
                "permission_required",
                "tool_execution_failed",
                "tool_aborted",
                "tool_timeout",
                "result_too_large",
                "path_not_allowed",
                "file_not_found",
                "file_conflict",
                "unsupported_tool",
                "setup_required",
                "plan_mode_violation",
                "ask_mode_violation",
            ]
            .iter()
            .any(|known| s.equals_str(known))
        })
        .cloned()
        .unwrap_or_else(|| "tool_execution_failed".into());
    let raw_error = response.get("error").unwrap_or(&undefined);
    let message = nullish(raw_error.get("message"))
        .map(js_string)
        .transpose()?
        .unwrap_or_else(|| "Capability module failed.".into());
    let mut error = obj([("code", s(&code)), ("message", s(&message))]);
    if wire_truthy(raw_code) || wire_truthy(raw_error) {
        let mut details = JsObject::new();
        if wire_truthy(raw_code) {
            put(&mut details, "moduleCode", raw_code.clone());
        }
        if wire_truthy(raw_error) {
            put(&mut details, "moduleError", raw_error.clone());
        }
        put(&mut error, "details", JsValue::Object(details));
    }
    let content = JsValue::Array(vec![JsValue::Object(obj([
        ("type", "text".into()),
        ("text", s(&message)),
    ]))]);
    let result = JsValue::Object(obj([
        ("type", "error".into()),
        ("toolCallId", s(&tool.tool_call_id)),
        ("toolName", s(&tool.name)),
        ("error", JsValue::Object(error)),
        ("content", content),
        ("startedAt", s(&exchange.wall_now_iso())),
        ("completedAt", s(&exchange.wall_now_iso())),
    ]));
    a::tool_result(&result)
}
fn transport_failure_tool_result<E: HostExchange>(
    exchange: &E,
    tool: &ToolInvocation,
    message: &JsString,
    module_code: &JsString,
) -> PortResult<pilotdeck_agent::ToolExecutionResult> {
    // Stock stdio callModule records a known failure before returning it. A
    // String TypeError there is caught by hostToolPort as a transport exception,
    // producing ordinary results with plain content, not missing-result advice.
    let error = obj([
        (
            "code",
            if module_code.equals_str("MODULE_TIMEOUT") {
                "tool_timeout".into()
            } else {
                "tool_execution_failed".into()
            },
        ),
        ("message", s(message)),
        (
            "details",
            JsValue::Object(obj([
                ("moduleCode", s(module_code)),
                ("outcome", "result_unknown".into()),
                ("retryability", "unsafe".into()),
            ])),
        ),
    ]);
    let result = JsValue::Object(obj([
        ("type", "error".into()),
        ("toolCallId", s(&tool.tool_call_id)),
        ("toolName", s(&tool.name)),
        ("error", JsValue::Object(error)),
        (
            "content",
            JsValue::Array(vec![JsValue::Object(obj([
                ("type", "text".into()),
                ("text", s(message)),
            ]))]),
        ),
        ("startedAt", s(&exchange.wall_now_iso())),
        ("completedAt", s(&exchange.wall_now_iso())),
    ]));
    a::tool_result(&result)
}
// Original stock hostToolPort validates a shallow completed batch before
// typed domain projection. These are known consumer exceptions, not lost ACKs.
fn batch_consumer_exception(response: &JsValue, calls: &[ToolInvocation]) -> Option<JsString> {
    let Some(JsValue::Array(results)) = response.get("payload").and_then(|p| p.get("results"))
    else {
        return Some(
            "Capability batch response must contain one result for every tool call.".into(),
        );
    };
    if results.len() != calls.len() {
        return Some(
            "Capability batch response must contain one result for every tool call.".into(),
        );
    }
    for (index, (result, call)) in results.iter().zip(calls).enumerate() {
        if !matches!(result, JsValue::Object(object) if object.contains_key("type")) {
            return Some(format!("Capability batch result {index} is invalid.").into());
        }
        if result.get("toolCallId") != Some(&JsValue::String(call.tool_call_id.clone())) {
            let mut message = format!("Capability batch result {index} does not match tool call ")
                .encode_utf16()
                .collect::<Vec<_>>();
            message.extend_from_slice(call.tool_call_id.units());
            message.push(46);
            return Some(JsString::from_utf16(message));
        }
    }
    None
}
pub(crate) fn call<E: HostExchange>(
    exchange: &mut E,
    methods: &AdvertisedMethods,
    control: &CallControl,
    call: HostCall,
) -> PortResult<HostReply> {
    control.check()?;
    let (module, operation) = call.method();
    if !methods.supports(module, operation) {
        return Err(PortError::NotAdvertised { module, operation });
    }
    let exchanged = exchange.exchange(control, module, operation, call.effect(), payload(&call))?;
    let response = exchanged.boundary.value();
    let unresolved = || PortError::AcceptedUnknown {
        identity: Box::new(exchanged.identity.clone()),
        last_applied_sequence: response
            .get("lastAppliedSequence")
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0),
    };
    let admission_error = |error| {
        if call.effect() == pilotdeck_protocol::admission::CallEffect::ReadOnly {
            error
        } else {
            unresolved()
        }
    };
    let envelope =
        EnvelopeRef::decode(response).map_err(|e| admission_error(a::invalid(e.message)))?;
    let MessageRef::Response(wire) = envelope.message else {
        return Err(admission_error(a::invalid(
            "Host exchange did not return ModuleResponse",
        )));
    };
    let unknown = wire.outcome == Some(pilotdeck_protocol::module::ModuleOutcome::ResultUnknown)
        || matches!(
            &call,
            HostCall::ExecuteSingle { .. } | HostCall::ExecuteBatch { .. }
        ) && (tool_result_unknown(response.get("payload").unwrap_or(&JsValue::Undefined))
            || response
                .get("payload")
                .and_then(|p| p.get("results"))
                .and_then(|v| {
                    if let JsValue::Array(a) = v {
                        Some(a)
                    } else {
                        None
                    }
                })
                .is_some_and(|a| a.iter().any(tool_result_unknown)));
    if unknown {
        return Err(PortError::AcceptedUnknown {
            identity: Box::new(exchanged.identity),
            last_applied_sequence: response
                .get("lastAppliedSequence")
                .and_then(JsValue::as_number)
                .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0),
        });
    }
    if matches!(&call, HostCall::ExecuteBatch { .. })
        && let Some(cause @ (CancelCause::OwnerRetired | CancelCause::TransportClosed)) =
            control.cancellation.cause()
    {
        // Retirement is never a recoverable tool consumer exception. A known
        // reply settles responsibility but cannot grant further observer calls.
        return Err(PortError::Cancelled { cause });
    }
    if matches!(&call, HostCall::ExecuteBatch { .. }) && exchanged.cancelled_before_response {
        // The response still settles this accepted identity. Stock stdio's
        // abort listener already rejected its consumer promise before ACK, so
        // no observer runs. ACK-first is retained even if the worker wakes late.
        return Ok(HostReply::ToolBatchRejected {
            message: "Tool execution cancelled.".into(),
            boundary: exchanged.boundary.clone(),
            identity: Box::new(exchanged.identity.clone()),
        });
    }
    let explicit_terminal_failure = !wire.ok
        && module == "capability"
        && response.get("final") == Some(&JsValue::Bool(true))
        && wire.outcome == Some(pilotdeck_protocol::module::ModuleOutcome::Failed);
    if let HostCall::ExecuteSingle { .. } = &call
        && !explicit_terminal_failure
    {
        let undefined = JsValue::Undefined;
        let error_value = if wire.ok {
            response.get("payload").unwrap_or(&undefined)
        } else {
            response
        };
        if cancelled_tool_result(error_value, !wire.ok)? {
            return Err(PortError::Cancelled {
                cause: CancelCause::Requested("tool_cancelled".into()),
            });
        }
    }
    if !wire.ok {
        if explicit_terminal_failure {
            // Source callModule constructs this Error BEFORE recordFailure.
            // Code is copied only after String(message ?? fallback) succeeds.
            let (message, string_failed) =
                match nullish(response.get("error").and_then(|e| e.get("message")))
                    .map(js_string)
                    .transpose()
                {
                    Ok(value) => (
                        value.unwrap_or_else(|| "Host module execution failed.".into()),
                        false,
                    ),
                    Err(PortError::InvalidResponse { message })
                        if message.equals_str(
                            "JS object primitive conversion fails with own non-callable toString",
                        ) =>
                    {
                        ("Cannot convert object to primitive value".into(), true)
                    }
                    Err(error) => return Err(error),
                };
            let code = (!string_failed)
                .then(|| response.get("code").and_then(JsValue::as_string).cloned())
                .flatten();
            if matches!(
                &call,
                HostCall::ExecuteBatch { .. } | HostCall::ExecuteSingle { .. }
            ) {
                if code
                    .as_ref()
                    .is_some_and(|code| code.equals_str("MODULE_ABORTED"))
                {
                    control
                        .cancellation
                        .cancel(CancelCause::Requested("tool_cancelled".into()));
                }
                match control.check() {
                    Err(PortError::Cancelled {
                        cause: cause @ (CancelCause::OwnerRetired | CancelCause::TransportClosed),
                    }) => {
                        return Err(PortError::Cancelled { cause });
                    }
                    Err(PortError::Cancelled {
                        cause: cause @ (CancelCause::Requested(_) | CancelCause::Deadline),
                    }) => {
                        if matches!(&call, HostCall::ExecuteBatch { .. }) {
                            return Ok(HostReply::ToolBatchRejected {
                                message: "Tool execution cancelled.".into(),
                                boundary: exchanged.boundary.clone(),
                                identity: Box::new(exchanged.identity.clone()),
                            });
                        }
                        return Err(PortError::Cancelled { cause });
                    }
                    Err(error) => return Err(error),
                    Ok(()) => {}
                }
                let module_code = code.unwrap_or_else(|| "MODULE_TRANSPORT_UNAVAILABLE".into());
                return match &call {
                    HostCall::ExecuteBatch { calls, .. } => calls
                        .iter()
                        .map(|tool| {
                            transport_failure_tool_result(exchange, tool, &message, &module_code)
                        })
                        .collect::<PortResult<Vec<_>>>()
                        .map(HostReply::ToolResults),
                    HostCall::ExecuteSingle { call, .. } => {
                        transport_failure_tool_result(exchange, call, &message, &module_code)
                            .map(Box::new)
                            .map(HostReply::ToolResult)
                    }
                    _ => unreachable!(),
                };
            }
            return Err(if string_failed {
                PortError::KnownConsumerException {
                    message,
                    boundary: exchanged.boundary.clone(),
                }
            } else {
                PortError::KnownTerminalException {
                    message,
                    code,
                    boundary: exchanged.boundary.clone(),
                }
            });
        }
        if let HostCall::ExecuteBatch { calls, .. } = &call {
            // agentLoopSidecarServer.callModule's recordFailure conversion
            // precedes the capability consumer and includes the raw code in
            // its nullish fallback. Detect only the represented String TypeError.
            let failure_message = nullish(response.get("error").and_then(|e| e.get("message")))
                .or_else(|| nullish(response.get("code")));
            match failure_message.map(js_string).transpose() {
                Ok(_) => {}
                Err(PortError::InvalidResponse { message })
                    if message.equals_str(
                        "JS object primitive conversion fails with own non-callable toString",
                    ) =>
                {
                    // Unlike a normal resolved result, this path passes through
                    // hostToolPort's catch. Source checks the current abort signal
                    // there even when its ACK promise resolved before cancellation.
                    match control.check() {
                        Err(PortError::Cancelled {
                            cause:
                                cause @ (CancelCause::OwnerRetired | CancelCause::TransportClosed),
                        }) => return Err(PortError::Cancelled { cause }),
                        Err(PortError::Cancelled {
                            cause: CancelCause::Requested(_) | CancelCause::Deadline,
                        }) => {
                            return Ok(HostReply::ToolBatchRejected {
                                message: "Tool execution cancelled.".into(),
                                boundary: exchanged.boundary.clone(),
                                identity: Box::new(exchanged.identity.clone()),
                            });
                        }
                        Err(error) => return Err(error),
                        Ok(()) => {}
                    }
                    let message = "Cannot convert object to primitive value".into();
                    return calls
                        .iter()
                        .map(|tool| {
                            transport_failure_tool_result(
                                exchange,
                                tool,
                                &message,
                                &"MODULE_TRANSPORT_UNAVAILABLE".into(),
                            )
                        })
                        .collect::<PortResult<Vec<_>>>()
                        .map(HostReply::ToolResults);
                }
                Err(error) => return Err(error),
            }
            return calls
                .iter()
                .map(|tool| module_failure_tool_result(exchange, response, tool))
                .collect::<PortResult<Vec<_>>>()
                .map(HostReply::ToolResults);
        }
        if matches!(
            &call,
            HostCall::PlanTodo(PlanTodoRequest {
                mutation: PlanTodoMutation::Read,
                ..
            })
        ) {
            // Source constructs Error(String(message ?? raw code ?? fallback))
            // then assigns RAW response.code. Never stringify code separately.
            let raw_message = nullish(response.get("error").and_then(|e| e.get("message")))
                .or_else(|| nullish(response.get("code")));
            let (message, string_failed) = match raw_message.map(js_string).transpose() {
                Ok(value) => (
                    value.unwrap_or_else(|| "Plan/todo module failed.".into()),
                    false,
                ),
                Err(PortError::InvalidResponse { message })
                    if message.equals_str(
                        "JS object primitive conversion fails with own non-callable toString",
                    ) =>
                {
                    ("Cannot convert object to primitive value".into(), true)
                }
                Err(error) => return Err(error),
            };
            // Only a successfully constructed Error receives the raw code.
            // A String TypeError itself has no copied code or terminal property.
            if !string_failed
                && response
                    .get("code")
                    .and_then(JsValue::as_string)
                    .is_some_and(|code| code.equals_str("RESULT_UNKNOWN"))
            {
                return Err(unresolved());
            }
            return Err(PortError::KnownConsumerException {
                message,
                boundary: exchanged.boundary.clone(),
            });
        }
        return Err(failure(response, &call)?);
    }
    if let HostCall::ExecuteBatch { calls, .. } = &call
        && let Some(message) = batch_consumer_exception(response, calls)
    {
        return Ok(HostReply::ToolBatchRejected {
            message,
            boundary: exchanged.boundary.clone(),
            identity: Box::new(exchanged.identity.clone()),
        });
    }
    if matches!(call, HostCall::ModelPrepare { .. })
        && methods.supports("model", "get_metadata")
        && matches!(
            response.get("payload").and_then(|p| p.get("metadata")),
            None | Some(JsValue::Undefined)
        )
    {
        return Err(admission_error(a::invalid(
            "Advertised model metadata callback requires prepare metadata",
        )));
    }
    let reply = reply(response, &call).map_err(admission_error)?;
    if !call.accepts(&reply) {
        return Err(admission_error(a::invalid(
            "HostReply is not associated with HostCall",
        )));
    }
    Ok(reply)
}
