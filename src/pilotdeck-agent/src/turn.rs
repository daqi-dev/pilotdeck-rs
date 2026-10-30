//! Owning AgentLoop turn policy. IO/identity/serialization remain bound ports.
use crate::ports::{PermissionMode, Route, RuntimeContextSurface};
use pilotdeck_protocol::compaction::SUMMARY_PREFIX as COMPACT_SUMMARY_PREFIX;
use pilotdeck_protocol::model::{BoundarySnapshot, ThinkingConfig, ToolChoice};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Agent,
    Plan,
    Ask,
}
#[derive(Debug, Clone)]
pub struct TurnModelOverride {
    pub route: Route,
    pub temperature: Option<f64>,
    pub speed: Option<f64>,
    pub thinking: Option<ThinkingConfig>,
}
#[derive(Debug, Clone)]
pub struct TurnSubagentModel {
    pub route: Route,
    pub max_context_tokens: Option<f64>,
    pub max_output_tokens: Option<f64>,
}
/// Rules are opaque immutable wire entries until the permission owner admits
/// them. Original factory accepts array entries without inspecting each rule.
#[derive(Debug, Clone)]
pub struct TurnPermissionRules {
    pub allow: Option<Vec<JsValue>>,
    pub deny: Option<Vec<JsValue>>,
    pub ask: Option<Vec<JsValue>>,
}
#[derive(Debug, Clone)]
pub struct TurnPermissionContext {
    pub mode: PermissionMode,
    pub can_prompt: bool,
    pub bypass_available: bool,
    pub additional_working_directories: Vec<JsString>,
    pub rules: Option<TurnPermissionRules>,
}
#[derive(Debug, Clone)]
pub struct TurnRuntimeConfig {
    pub route: Route,
    pub cwd: JsString,
    pub system_prompt: Option<JsString>,
    pub append_system_prompt: Option<JsString>,
    pub plan_mode_instructions: Option<JsString>,
    pub runtime_context_surface: RuntimeContextSurface,
    pub max_output_tokens: Option<f64>,
    pub max_context_tokens: Option<f64>,
    pub thinking: Option<ThinkingConfig>,
    pub tool_choice: Option<ToolChoice>,
    pub max_context_messages: Option<f64>,
    pub stop_on_structured_output: Option<bool>,
    pub json_self_correct: Option<bool>,
    pub run_mode: Option<RunMode>,
    pub is_subagent: Option<bool>,
    pub subagent_model: Option<TurnSubagentModel>,
    pub permission_mode_before_plan: Option<PermissionMode>,
    pub permission: TurnPermissionContext,
    /// Original shallow Object.assign merge; never a mutable Session aggregate.
    pub metadata: Option<JsObject>,
}
#[derive(Debug, Clone)]
pub struct TurnInput {
    pub session_id: JsString,
    pub turn_id: JsString,
    /// Fresh canonical values produced by the original factory's filtering rules.
    /// These are not arbitrary incoming messages or reconstructed typed snapshots.
    pub messages: Vec<BoundarySnapshot>,
    pub max_turns: Option<f64>,
    pub max_budget_usd: Option<f64>,
    pub task_budget_usd: Option<f64>,
    pub initial_task_budget_spent_usd: Option<f64>,
    pub base_permission_mode: Option<PermissionMode>,
    pub allow_plan_mode_tools: bool,
    pub can_elicit: bool,
    pub model_override: Option<TurnModelOverride>,
}
pub struct RoutedCompactionPreparation<'a> {
    pub config: &'a TurnRuntimeConfig,
    pub input: &'a TurnInput,
    pub messages: &'a [ProjectedMessage],
    pub prepared: &'a crate::ports::PreparedInvocation,
    pub base_request: &'a CapturedRequest,
}
/// Original model-error recovery compaction (AgentLoop.ts:1512-1544). The
/// `target` is `modelErrorTarget(error, decision)`; limits are read after the
/// branch applied its transient caps (A:1514-1515). The budget request is the
/// preview request with provider/model replaced by the target (A:2473-2502,
/// `decision` branch: no prepared invocation is materialized).
pub struct RecoveryCompactionPreparation<'a> {
    pub config: &'a TurnRuntimeConfig,
    pub input: &'a TurnInput,
    pub messages: &'a [ProjectedMessage],
    pub target: &'a Route,
}

use crate::driver::{BudgetLedger, InvocationDisposition, LiveModelEvent};
use crate::ports::{
    CallControl, CancelCause, CapturedRequest, HostCall, HostPort, HostReply, ModelHints,
    PortError, PortResult, PreparedRequestHandle, ToolHints, ToolInvocation,
};
use crate::source_invocation::run_source_model_invocation;
use crate::source_model::{SourceAssemblyError, SourceField};
use crate::{
    MessageProjection, ProjectedMessage, ToolExecutionResult, ToolResultDisposition, TurnTimeline,
    project_tool_results,
};
use pilotdeck_protocol::model::{ToolCall, Usage};

/// Each collaborator has one responsibility. Request assembly does not own the
/// output sink or the host Session; durability is an explicit separate fence.
/// Source merges one supplied cap field; it does not erase unrelated fields.
/// Requested/attempt caps are turn-local, while Session caps survive modelState.
/// `HardCap` is Original `hardMaxOutputTokens` (AgentLoop.ts:1449-1451,
/// 2587-2601): a reactive `adjust_output_and_retry` with scope `hard_cap` or
/// no scope merges it; it is never cleared by `clearTurnScopedTokenCaps`
/// (AgentLoop.ts:360) and participates in `currentMaxOutputTokens` as the
/// min candidate alongside the requested chain and the catalog limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputCapUpdate {
    Requested(f64),
    Session(f64),
    Attempt(f64),
    HardCap(f64),
}
/// Original `token_cap_adjusted.cap` (events.ts:81-91).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenCapKind {
    Output,
    Context,
}
/// Turn-owned immutable source projection. Error/finish decisions read the
/// preserved boundary; no closed ModelError normalization gates their entry.
#[derive(Debug, Clone)]
pub struct TurnAssembly {
    pub boundary: BoundarySnapshot,
    pub message: ProjectedMessage,
    pub usage: Option<Usage>,
    pub has_repaired_tool_calls: Option<bool>,
    pub has_message_end: bool,
}
pub trait TurnPreparationPort {
    /// Tool-call typed admission occurs only when the owning turn dispatches
    /// tools, after all pre-emission recovery gates have declined the response.
    fn project_tool_calls(&self, _calls: &[BoundarySnapshot]) -> PortResult<Vec<ToolCall>> {
        Err(profile_gap("open turn tool-call dispatch projection"))
    }
    /// Re-admit the complete boundary after Source repeated-failure annotation.
    /// The projection must describe the same value that will be published and
    /// passed to context; stale pre-annotation content is never reused.
    fn project_tool_result(&self, _result: &BoundarySnapshot) -> PortResult<ToolExecutionResult> {
        Err(profile_gap("annotated tool-result projection"))
    }
    fn current_output_cap(
        &self,
        _config: &TurnRuntimeConfig,
        _route: &Route,
    ) -> PortResult<Option<f64>> {
        Err(profile_gap("current output cap recovery lookup"))
    }
    /// Cached provider model limit, distinct from turn/session requested caps.
    fn model_output_limit(&self, _route: &Route) -> Option<f64> {
        None
    }
    fn set_output_cap(&mut self, _route: &Route, _update: OutputCapUpdate) -> PortResult<()> {
        Err(profile_gap("transient output cap recovery update"))
    }
    /// Original `setTransientTokenCap(target, { maxContextTokens })`
    /// (AgentLoop.ts:1480, merge 2633-2637). `clearTurnScopedTokenCaps`
    /// (A:2648-2660) keeps `maxContextTokens`, so like Session/HardCap output
    /// caps it survives the turn; `currentMaxContextTokens` (A:2570-2578) reads
    /// it first, which `compaction_token_limits` already mirrors.
    fn set_context_cap(&mut self, _route: &Route, _max_context_tokens: f64) -> PortResult<()> {
        Err(profile_gap("transient context cap recovery update"))
    }

    /// Source limit lookup is independent of the fallible preview construction.
    fn compaction_token_limits(
        &self,
        config: &TurnRuntimeConfig,
        _route: &crate::ports::Route,
        pre_route: bool,
    ) -> (f64, f64) {
        let subagent = config.is_subagent == Some(true) && config.subagent_model.is_some();
        (
            if pre_route && subagent {
                1_000_000.0
            } else {
                config.max_context_tokens.unwrap_or(1_000_000.0)
            },
            config.max_output_tokens.unwrap_or(0.0),
        )
    }
    /// Exact executed-request matching is owned by the turn. This port updates
    /// only its volatile model projection; it performs no host effect.
    fn record_token_calibration(
        &mut self,
        _route: &Route,
        _usage: Option<&BoundarySnapshot>,
        _estimated_input_tokens: Option<f64>,
    ) {
    }
    /// Source invalidates route calibration immediately after replacement,
    /// independently of the subsequent persistence callback outcome.
    fn clear_compaction_calibration(&mut self) {}
    /// Serializable preview intent. The owning turn, rather than request
    /// assembly, invokes compaction and enforces replacement durability.
    fn prepare_compaction(
        &mut self,
        _host: &mut dyn HostPort,
        _control: &CallControl,
        _config: &TurnRuntimeConfig,
        _input: &TurnInput,
        _messages: &[ProjectedMessage],
    ) -> PortResult<Option<Box<crate::ports::CompactionIntent>>> {
        Ok(None)
    }
    /// A routed preview includes the prepared provider controls and current
    /// canonical prompt window. Only the owning turn dispatches its compactor.
    fn prepare_routed_compaction(
        &mut self,
        _host: &mut dyn HostPort,
        _control: &CallControl,
        _parameters: RoutedCompactionPreparation<'_>,
    ) -> PortResult<Option<Box<crate::ports::CompactionIntent>>> {
        Err(profile_gap("routed compaction preview"))
    }
    /// Recovery-stage preview intent for the `compact_and_retry` branch
    /// (AgentLoop.ts:1521-1544): `budgetStage:"recovery"`,
    /// `maxContextTokens = currentMaxContextTokens(target)`,
    /// `reservedOutputTokens = currentMaxOutputTokens(target) ?? 0` (A:2550-2555),
    /// `allowFallbackOnFailure:true`. `Ok(None)` means the host has no
    /// `try_auto_compact` (A:1512 `ctx?.tryAutoCompact` absent); the owning turn
    /// then truncates 0.5 (A:1568-1571). Only the owning turn dispatches it.
    fn prepare_recovery_compaction(
        &mut self,
        _host: &mut dyn HostPort,
        _control: &CallControl,
        _parameters: RecoveryCompactionPreparation<'_>,
    ) -> PortResult<Option<Box<crate::ports::CompactionIntent>>> {
        Err(profile_gap("recovery compaction preview"))
    }
    fn rebuild_routed_request(
        &mut self,
        _host: &mut dyn HostPort,
        _control: &CallControl,
        _config: &TurnRuntimeConfig,
        _input: &TurnInput,
        _messages: &[ProjectedMessage],
        _prepared: &crate::ports::PreparedInvocation,
    ) -> PortResult<CapturedRequest> {
        Err(profile_gap("routed compaction request rebuild"))
    }
    /// Applies only AgentLoop-owned output caps; provider-specific prepared
    /// controls remain intact. The default preserves collaborator compatibility.
    fn cap_prepared_request(
        &self,
        _config: &TurnRuntimeConfig,
        prepared: &crate::ports::PreparedInvocation,
    ) -> PortResult<CapturedRequest> {
        Ok(prepared.request.clone())
    }
    fn clear_attempt_output_cap(&mut self, _route: &crate::ports::Route) {}
    /// Original host caches prepared metadata before AgentLoop resolves routed
    /// limits. Preparation owns the corresponding immutable read projection.
    /// Source starts this advisory lifecycle without awaiting its reply.
    fn before_model_request(&mut self, _route: &Route) -> PortResult<()> {
        Ok(())
    }
    fn observe_prepared_metadata(&mut self, _prepared: &crate::ports::PreparedInvocation) {}
    /// Resolve previously accepted advisory effects before a known terminal
    /// result. Cancellation does not erase their identity or permit replay.
    fn finish_pending_effects(&mut self) -> PortResult<()> {
        Ok(())
    }
    fn prepare(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        config: &TurnRuntimeConfig,
        input: &TurnInput,
        messages: &[ProjectedMessage],
    ) -> PortResult<(CapturedRequest, ModelHints)>;
    /// Pure boundary admission; it neither advances the host nor authorizes tools.
    fn project_assembly(&self, boundary: &BoundarySnapshot) -> PortResult<TurnAssembly>;
    /// Optional composition-owned observer, after successful tool execution and
    /// before publishing results. A PlanTodo manager alone does not enable it.
    fn observe_tool_results(
        &mut self,
        _host: &mut dyn HostPort,
        _control: &CallControl,
        _input: &TurnInput,
        _results: &[ToolExecutionResult],
    ) -> PortResult<()> {
        Ok(())
    }
}
pub trait TurnEventPort {
    fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()>;
    /// Already accepted model data and its final aborted partial reply remain
    /// publishable after user cancellation. Transport owns retirement/backpressure.
    fn emit_accepted(&mut self, event: BoundarySnapshot) -> PortResult<()> {
        self.emit(event)
    }
}
pub trait TurnClock {
    fn now_iso(&self) -> JsString;
    fn uuid(&self) -> JsString;
}
#[derive(Debug, Clone)]
pub enum TurnEvent {
    ContextBudget {
        snapshot: BoundarySnapshot,
    },
    RecoveryCap {
        route: Route,
        change: crate::output_recovery::RecoveryCapChange,
        finish_reason: Option<BoundarySnapshot>,
    },
    /// Reactive `token_cap_adjusted` (AgentLoop.ts:1461-1471, 1483-1512) whose
    /// provider/model is the `modelErrorTarget` and whose reason is the host
    /// decision reason. The residual max-output bump keeps `RecoveryCap`.
    TokenCapAdjusted {
        route: Route,
        cap: TokenCapKind,
        previous: Option<f64>,
        next: f64,
        reason: JsString,
    },
    ModelRequestStarted {
        route: Route,
    },
    Model {
        live: LiveModelEvent,
    },
    AssistantMessage {
        message: ProjectedMessage,
    },
    ToolCallsDetected {
        calls: Vec<ToolCall>,
        boundaries: Vec<BoundarySnapshot>,
    },
    ToolResult {
        result: ToolExecutionResult,
    },
    ToolResultsProjected {
        message: ProjectedMessage,
    },
    ModeChangeRequested {
        mode: PermissionMode,
    },
    SteerApplied {
        item_id: JsString,
        message: ProjectedMessage,
    },
    Continued {
        reason: TurnContinueReason,
    },
    StopRequested,
    StopFailure {
        error: JsString,
    },
    StopFailureBoundary {
        error: BoundarySnapshot,
    },
    AgentStatus {
        event: JsString,
        detail: BoundarySnapshot,
    },
    Failed {
        error: BoundarySnapshot,
    },
    Completed {
        result: TurnResult,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnContinueReason {
    ToolResults,
    UserSteer,
    AutoCompact,
    ModelError,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnResultKind {
    Success,
    Error,
    Aborted,
    MaxTurns,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStopReason {
    Completed,
    MaxTurns,
    MaxBudget,
    TaskBudget,
    AbortedStreaming,
    AbortedTools,
    ModelError,
    PromptTooLong,
    ToolError,
    UnsupportedRecovery,
}
#[derive(Debug, Clone)]
pub struct TurnPermissionDenial {
    pub tool_name: JsString,
    pub tool_call_id: JsString,
    pub error_code: Option<JsString>,
}
#[derive(Debug, Clone)]
pub struct TurnResult {
    pub kind: TurnResultKind,
    pub stop_reason: TurnStopReason,
    pub final_message: Option<ProjectedMessage>,
    pub usage: Usage,
    pub permission_denials: Vec<TurnPermissionDenial>,
    pub turns: f64,
    pub started_at: JsString,
    pub completed_at: JsString,
    pub structured_output: Option<BoundarySnapshot>,
    pub errors: Vec<BoundarySnapshot>,
    pub budget: BudgetLedger,
}
#[derive(Debug, Clone)]
pub struct TurnOutput {
    pub result: TurnResult,
    pub messages: Vec<ProjectedMessage>,
}
/// Turn-local state has closed responsibilities; immutable boundaries retain
/// admitted message/result extensions, never host authority or native handles.
#[derive(Debug, Clone)]
pub struct TurnExecutionState {
    pub messages: Vec<ProjectedMessage>,
    pub turn_count: f64,
    pub usage: Usage,
    pub permission_denials: Vec<TurnPermissionDenial>,
    pub final_message: Option<ProjectedMessage>,
    pub structured_output: Option<BoundarySnapshot>,
    pub budget: BudgetLedger,
    pub timeline: TurnTimeline,
    transient_prompt_ids: Vec<JsString>,
}

/// Actual normal-path owning turn loop. The recovery decision table, budget
/// terminal wording, progress pump and live seed/model checkpoints are still
/// explicit unimplemented profiles. They fail visibly before further effects;
/// this entry is wired through the native factory but is not the full CORE gate.
#[allow(clippy::too_many_arguments)]
pub fn run_admitted_turn(
    config: &mut TurnRuntimeConfig,
    input: &TurnInput,
    initial: Vec<ProjectedMessage>,
    host: &mut dyn HostPort,
    preparation: &mut dyn TurnPreparationPort,
    events: &mut dyn TurnEventPort,
    clock: &dyn TurnClock,
    control: &CallControl,
    cleanup: &CallControl,
    tool_hints: &mut ToolHints,
) -> PortResult<TurnOutput> {
    let started_at = clock.now_iso();
    let mut state = TurnExecutionState {
        messages: initial,
        turn_count: 1.0,
        usage: empty_usage(),
        permission_denials: Vec::new(),
        final_message: None,
        structured_output: None,
        budget: BudgetLedger::new(
            input.max_budget_usd,
            input.task_budget_usd,
            input.initial_task_budget_spent_usd.unwrap_or(0.0),
        ),
        timeline: TurnTimeline::new(input.turn_id.clone()),
        transient_prompt_ids: Vec::new(),
    };
    let mut early_stream_recovery = crate::early_stream_recovery::EarlyStreamRecovery::new();
    let mut output_recovery = crate::output_recovery::OutputRecovery::new();
    let mut large_file_repair = crate::LargeFileRepair::new();
    let mut tool_failure_recovery = crate::tool_failures::ToolFailureRecovery::new();
    // Original AgentLoop.ts:460-461 `jsonSelfCorrectCount` against
    // MAX_JSON_SELF_CORRECT_RETRIES=3; the policy owns the comparison.
    let mut json_self_correct_count: u32 = 0;
    // Original AgentLoop.ts:436 `hasAttemptedCompact`: set by the reactive
    // truncate/compact branches (A:1572, A:1589), forwarded to every later
    // recover_from_model_error probe (A:1444), never reset within the run.
    let mut has_attempted_compact = false;
    // Original AgentLoop.ts:462 `hasAttemptedReasoningContentRetry`: set once
    // at A:1391, never reset within the run.
    let mut has_attempted_reasoning_content_retry = false;
    // Original AgentLoop.ts:464 `hasAttemptedImageStrip`: set at A:1600, reset
    // only after a clean tool round (A:2189) together with the output counters.
    let mut has_attempted_image_strip = false;
    loop {
        if let Err(error) = control.check() {
            return finish_pre_model_abort(
                state,
                started_at,
                error,
                host,
                preparation,
                events,
                clock,
                input,
                cleanup,
            );
        }
        if host.advertised("turn", "drain_steer") {
            let HostReply::SteerMessages(steers) = host.call(control, HostCall::TurnDrain)? else {
                return Err(wrong_reply("drain_steer"));
            };
            for mut steer in steers {
                // The authenticated host mailbox returns already committed items;
                // Session/TurnRunner, not AgentLoop, owns canonical persistence.
                control.check()?;
                annotate_message(&mut state.timeline, &mut steer.message)?;
                state.messages.push(steer.message.clone());
                emit_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::SteerApplied {
                        item_id: steer.item_id,
                        message: steer.message,
                    },
                )?;
            }
        }
        let pre_route_max_context_tokens = preparation
            .compaction_token_limits(config, &config.route, true)
            .0;
        let pre_route_step =
            match preparation.prepare_compaction(host, control, config, input, &state.messages) {
                Ok(Some(intent)) => match host.call(control, HostCall::AutoCompact(intent)) {
                    Ok(HostReply::Compacted(compact)) => install_pre_route_compaction(
                        &compact.boundary,
                        &mut state,
                        host,
                        preparation,
                        events,
                        input,
                        control,
                    )?,
                    Ok(HostReply::CompactedBoundaryOnly(boundary)) => install_pre_route_compaction(
                        &boundary,
                        &mut state,
                        host,
                        preparation,
                        events,
                        input,
                        control,
                    )?,
                    Ok(_) => return Err(wrong_reply("try_auto_compact")),
                    Err(error) if known_compaction_advisory(&error) => {
                        CompactionStep::Snapshot(None)
                    }
                    Err(error) => return Err(error),
                },
                Ok(None) => CompactionStep::Snapshot(None),
                Err(error) if known_compaction_advisory(&error) => CompactionStep::Snapshot(None),
                Err(error) => return Err(error),
            };
        let mut pending_context_budget = match pre_route_step {
            CompactionStep::Snapshot(snapshot) => snapshot,
            CompactionStep::Emergency(compact) => {
                match finish_emergency_compaction(
                    &mut state,
                    started_at.clone(),
                    &compact,
                    clock,
                    TerminalTurnPorts {
                        host,
                        events,
                        input,
                        control,
                    },
                ) {
                    Ok(output) => return Ok(output),
                    // Preserve the original compaction catch after the earlier
                    // StopFailure/result phases; an unknown effect is fatal.
                    Err(error) if known_compaction_advisory(&error) => None,
                    Err(error) => return Err(error),
                }
            }
        };
        let (request, hints) =
            match preparation.prepare(host, control, config, input, &state.messages) {
                Ok(request) => request,
                Err(error) => {
                    return finish_pre_model_abort(
                        state,
                        started_at,
                        error,
                        host,
                        preparation,
                        events,
                        clock,
                        input,
                        cleanup,
                    );
                }
            };
        if let Err(error) = control.check() {
            return finish_pre_model_abort(
                state,
                started_at,
                error,
                host,
                preparation,
                events,
                clock,
                input,
                cleanup,
            );
        }
        preparation.before_model_request(&request.route)?;
        emit_event(
            events,
            &mut state.timeline,
            input,
            TurnEvent::ModelRequestStarted {
                route: request.route.clone(),
            },
        )?;
        let handle = PreparedRequestHandle(clock.uuid());
        let base_request = request.clone();
        let mut prepared = if host.advertised("model", "prepare") {
            let HostReply::ModelPrepared(prepared) = host.call(
                control,
                HostCall::ModelPrepare {
                    handle,
                    request: Box::new(request),
                    hints: hints.clone(),
                },
            )?
            else {
                return Err(wrong_reply("prepare_request"));
            };
            prepared
        } else {
            let mut boundary = JsObject::new();
            boundary.insert("request".into(), request.wire.request().clone());
            boundary.insert(
                "provider".into(),
                JsValue::String(request.route.provider.clone()),
            );
            boundary.insert("model".into(), JsValue::String(request.route.model.clone()));
            Box::new(crate::ports::PreparedInvocation {
                handle,
                route: request.route.clone(),
                request,
                max_context_tokens: None,
                max_output_tokens: None,
                metadata: None,
                boundary: BoundarySnapshot::capture(&JsValue::Object(boundary)),
            })
        };
        control.check()?;
        preparation.observe_prepared_metadata(&prepared);
        if host.advertised("context", "try_auto_compact")
            && prepared.max_context_tokens.unwrap_or_else(|| {
                preparation
                    .compaction_token_limits(config, &prepared.route, false)
                    .0
            }) != pre_route_max_context_tokens
        {
            match run_routed_compaction(
                &mut state,
                host,
                preparation,
                events,
                input,
                control,
                config,
                &mut prepared,
                &base_request,
            ) {
                Ok(CompactionStep::Snapshot(Some(snapshot))) => {
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::ContextBudget { snapshot },
                    )?;
                    pending_context_budget = None;
                }
                Ok(CompactionStep::Snapshot(None)) => {}
                Ok(CompactionStep::Emergency(compact)) => {
                    match finish_emergency_compaction(
                        &mut state,
                        started_at.clone(),
                        &compact,
                        clock,
                        TerminalTurnPorts {
                            host,
                            events,
                            input,
                            control,
                        },
                    ) {
                        Ok(output) => return Ok(output),
                        Err(error) if known_compaction_advisory(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(RoutedCompactionFailure::Advisory(error))
                    if known_compaction_advisory(&error) => {}
                Err(
                    RoutedCompactionFailure::Advisory(error)
                    | RoutedCompactionFailure::Fatal(error),
                ) => return Err(error),
            }
        }
        prepared.request = preparation.cap_prepared_request(config, &prepared)?;
        preparation.clear_attempt_output_cap(&prepared.route);
        if let Some(snapshot) = pending_context_budget {
            emit_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::ContextBudget { snapshot },
            )?;
        }
        // Source AgentLoop invokes the optional input estimator on the final
        // prepared/capped request, after context_budget and before stream start.
        // Retain its result for budget fallback and pending calibration;
        // absence of this capability remains distinct from an invented zero.
        let request_input_estimate = if host.advertised("budget", "estimate_request_input") {
            let HostReply::BudgetEstimate(tokens) = host.call(
                control,
                HostCall::BudgetEstimate {
                    request: Box::new(prepared.request.clone()),
                },
            )?
            else {
                return Err(wrong_reply("estimate_request_input"));
            };
            Some(tokens)
        } else {
            None
        };
        let calibration_fingerprint = source_request_fingerprint(&prepared.request)?;
        let invocation = run_source_model_invocation(
            host,
            control,
            cleanup,
            &prepared,
            &hints,
            clock.uuid(),
            &mut |live| {
                emit_accepted_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::Model { live },
                )
            },
        );
        // User cancellation cannot erase a dispatched operation whose effect
        // has not been resolved. Retain the original identity before aborting.
        if let InvocationDisposition::PortFailure(error) = &invocation.disposition
            && matches!(error.as_ref(), PortError::AcceptedUnknown { .. })
        {
            if is_deadline_unknown(error, control) {
                // TS :861 assembles before capture/completion. If assembly
                // itself throws (e.g. rawId.trim), no aborted fact follows;
                // preserve the accepted call's original uncertainty here.
                if let Ok(raw) = invocation.assembled {
                    // Aborted is a turn fact, not proof that the accepted model
                    // call settled. Publish the source catch's cleanup/events,
                    // then return the original uncertainty to its owner.
                    let _ = finish_aborted_stream(
                        state,
                        started_at,
                        raw,
                        CancelCause::Deadline,
                        AbortedStreamBranch::StreamThrew,
                        AbortedTurnPorts {
                            host,
                            preparation,
                            events,
                            clock,
                            input,
                            control,
                        },
                    );
                }
                return Err(error.as_ref().clone());
            }
            let mut capture_messages = state.messages.clone();
            if let Ok(assembled) = &invocation.assembled
                && let Some(partial) = safe_unknown_capture_partial(assembled)
            {
                capture_messages.push(partial);
            }
            return Err(capture_after_unknown_abort(
                host,
                control,
                input,
                &capture_messages,
                error.as_ref().clone(),
            ));
        }
        let cancelled = match control.check() {
            Err(PortError::Cancelled { cause }) => Some(cause),
            _ => match &invocation.disposition {
                InvocationDisposition::Cancelled(cause) => Some(cause.clone()),
                _ => None,
            },
        };
        if let Some(cause) = cancelled {
            // Source distinguishes two aborted-stream branches. AgentLoop.ts:859-885:
            // the model iterator threw (or was cancelled) while aborted -> no abort
            // status. AgentLoop.ts:916-946: the iterator completed normally and the
            // abort was observed afterwards -> abort status when the reason matches.
            // Only an iterator that ended without throwing reaches the second
            // branch. The disposition alone cannot select it: the invocation
            // re-checks control after the final batch and reports Cancelled for
            // a completed stream too (probe during_stream_eos requires 1 status).
            let branch = if invocation.stream_completed {
                AbortedStreamBranch::StreamCompleted
            } else {
                AbortedStreamBranch::StreamThrew
            };
            let raw = invocation
                .assembled
                .map_err(|error| source_assembly_error(error, &prepared.boundary))?;
            return finish_aborted_stream(
                state,
                started_at,
                raw,
                cause,
                branch,
                AbortedTurnPorts {
                    host,
                    preparation,
                    events,
                    clock,
                    input,
                    control: cleanup,
                },
            );
        }
        match invocation.disposition {
            InvocationDisposition::PortFailure(error) if invocation.plain_host_stream_throw => {
                let PortError::HostFailure { message, .. } = *error else {
                    return Err(wrong_reply("plain model stream throw classification"));
                };
                // Source's iterator catch does not append the partial assembly,
                // expire transient prompts, charge this invocation, or calibrate.
                return finish_plain_model_stream_throw(
                    state,
                    started_at,
                    message,
                    clock,
                    TerminalTurnPorts {
                        host,
                        events,
                        input,
                        control,
                    },
                );
            }
            InvocationDisposition::PortFailure(error) => return Err(*error),
            InvocationDisposition::Cancelled(cause) => return Err(PortError::Cancelled { cause }),
            // Source error events break/close the model iterator, then enter
            // the ordinary assembled-error recovery gates. They are distinct
            // from a failure of the bound stream operation itself.
            InvocationDisposition::BoundaryError { .. } | InvocationDisposition::Assembled => {}
        }
        control.check()?;
        let source_assembled = invocation.assembled.map_err(|error| {
            let message = match error {
                SourceAssemblyError::TypeError { message }
                | SourceAssemblyError::NativeGraphRequired { message } => message,
            };
            PortError::SourceException {
                message,
                boundary: prepared.boundary.clone(),
            }
        })?;
        // Match the exact final request, not just the advertised provider route.
        // Fallback/downgrade/continuation contents can change on the same route.
        if invocation.executed.as_ref().is_some_and(|executed| {
            source_field_equals(&executed.provider, &prepared.request.route.provider)
                && source_field_equals(&executed.model, &prepared.request.route.model)
                && source_field_equals(&executed.fingerprint, &calibration_fingerprint)
        }) {
            let original_usage = source_assembled
                .value()
                .get("usage")
                .map(BoundarySnapshot::capture);
            preparation.record_token_calibration(
                &prepared.request.route,
                original_usage.as_ref(),
                request_input_estimate,
            );
        }
        let assembled = preparation.project_assembly(&source_assembled)?;
        let mut message = assembled.message.clone();
        let source_tool_calls = collect_recovery_tool_calls(&message.boundary)?;
        let (budget_usage, budget_usage_view) = select_owning_budget_usage(
            &assembled,
            request_input_estimate,
            input.max_budget_usd.is_some() || input.task_budget_usd.is_some(),
        )?;
        merge_usage(&mut state.usage, budget_usage_view.as_ref());
        let actual_route = if let Some(executed) = &invocation.executed {
            Route {
                provider: executed_label(&executed.provider)?
                    .unwrap_or_else(|| prepared.route.provider.clone()),
                model: executed_label(&executed.model)?
                    .unwrap_or_else(|| prepared.route.model.clone()),
            }
        } else {
            prepared.route.clone()
        };
        // Source sets finalMessage and expires only the prompts consumed by
        // this invocation before entering the post-usage recovery gates.
        state.final_message = Some(message.clone());
        expire_consumed_transient_prompts(&mut state);
        let (invocation_cost_usd, budget_stop) = state.budget.charge_source_usage(
            host,
            control,
            budget_usage.as_ref(),
            actual_route.clone(),
        )?;
        if let Some(stop) = budget_stop {
            let plan = crate::budget_terminal::OriginalBudgetTerminalPolicy.evaluate(
                crate::budget_terminal::BudgetTerminalInput {
                    ledger: &state.budget,
                    stop,
                    last_invocation_cost_usd: invocation_cost_usd,
                    route: &actual_route,
                    assistant_message: &message,
                    unexecuted_tool_call_count: source_tool_calls.len(),
                },
            )?;
            return finish_budget_terminal(
                state,
                started_at,
                plan,
                clock,
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            );
        }
        let early_plan =
            early_stream_recovery.select(crate::early_stream_recovery::EarlyStreamInput {
                assembled: &source_assembled,
                assistant_message: &message.boundary,
                tool_calls: &source_tool_calls,
                has_message_end: assembled.has_message_end,
            })?;
        match early_plan.action {
            crate::early_stream_recovery::EarlyStreamAction::FallThrough => {}
            crate::early_stream_recovery::EarlyStreamAction::Retry {
                clear_final_message,
                ..
            } => {
                if clear_final_message {
                    state.final_message = None;
                }
                // Unknown retry projects withoutThinkingBlocks even when calls
                // make the resulting text ineligible. Do not skip this phase.
                if let Some(partial) = early_plan.partial_text_message()? {
                    append_recovery_assistant(&mut state, partial, events, input)?;
                }
                let (prompt, purpose) = early_plan.retry_prompt()?;
                push_transient_synthetic_prompt(&mut state, prompt, purpose, clock);
                emit_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::Continued {
                        reason: TurnContinueReason::ModelError,
                    },
                )?;
                continue;
            }
            crate::early_stream_recovery::EarlyStreamAction::Exhausted => {
                return finish_early_stream_exhausted(
                    state,
                    started_at,
                    early_plan,
                    clock,
                    TerminalTurnPorts {
                        host,
                        events,
                        input,
                        control,
                    },
                );
            }
        }
        let repaired_gate = source_assembled
            .value()
            .get("hasRepairedToolCalls")
            .is_some_and(wire_truthy)
            && source_assembled
                .value()
                .get("finishReason")
                .and_then(JsValue::as_string)
                .is_some_and(|r| {
                    r.equals_str("length") || r.equals_str("tool_call") || r.equals_str("stop")
                });
        let is_output_length = source_assembled
            .value()
            .get("finishReason")
            .and_then(JsValue::as_string)
            .is_some_and(|r| r.equals_str("length"));
        let needs_output_caps = repaired_gate
            || (is_output_length
                && !source_assembled
                    .value()
                    .get("error")
                    .is_some_and(wire_truthy)
                && source_tool_calls.is_empty()
                && message_text(message.boundary.value())?.is_empty());
        let current_output_cap = if needs_output_caps {
            preparation.current_output_cap(config, &prepared.route)?
        } else {
            None
        };
        let recovery_request = BoundarySnapshot::capture(prepared.request.wire.request());
        let output_plan = output_recovery.select(crate::output_recovery::OutputRecoveryInput {
            assembled: &source_assembled,
            assistant_message: &message.boundary,
            tool_calls: &source_tool_calls,
            has_repaired_tool_calls: source_assembled
                .value()
                .get("hasRepairedToolCalls")
                .is_some_and(wire_truthy),
            original_request: &recovery_request,
            routed_route: &prepared.route,
            current_max_output_tokens: current_output_cap,
            routed_model_max_output_tokens: prepared
                .max_output_tokens
                .or_else(|| preparation.model_output_limit(&prepared.route)),
        })?;
        let output_plan = match output_plan {
            crate::output_recovery::OutputRecoverySelection::Plan(plan) => plan,
            crate::output_recovery::OutputRecoverySelection::NeedsLargeFileRepair(token) => {
                let decision = large_file_repair
                    .recover_from_boundary_truncation(&source_tool_calls)
                    .map_err(|_| profile_gap("raw large-file repaired-call query"))?;
                token.resolve(decision)?
            }
        };
        match apply_pre_emission_output_plan(
            &output_plan,
            &mut state,
            &started_at,
            config,
            preparation,
            &large_file_repair,
            clock,
            TerminalTurnPorts {
                host,
                events,
                input,
                control,
            },
        )? {
            OutputPlanProgress::FallThrough => {}
            OutputPlanProgress::Continue => continue,
            OutputPlanProgress::Completed(output) => return Ok(*output),
        }
        annotate_message(&mut state.timeline, &mut message)?;
        state.final_message = Some(message.clone());
        state.messages.push(message.clone());
        emit_event(
            events,
            &mut state.timeline,
            input,
            TurnEvent::AssistantMessage {
                message: message.clone(),
            },
        )?;
        control.check()?;
        if let Some(raw_error) = source_assembled
            .value()
            .get("error")
            .filter(|error| wire_truthy(error))
        {
            let error = BoundarySnapshot::capture(raw_error);
            // Original AgentLoop.ts:1387-1400: one reasoning-content retry per
            // run, before tool-result projection and before any host probe.
            if !has_attempted_reasoning_content_retry && requires_reasoning_content_retry(&error)? {
                has_attempted_reasoning_content_retry = true;
                state.messages = add_empty_reasoning_content_markers(&state.messages)?;
                emit_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::Continued {
                        reason: TurnContinueReason::ModelError,
                    },
                )?;
                continue;
            }
            if !source_tool_calls.is_empty() {
                // Original AgentLoop.ts:1402-1418: every assembled call receives a
                // createMissingToolResult in call order, the results are
                // projected (projectToolResults), pushed, published once as
                // `tool_results_projected` with projected[0], then each projected
                // message is durably accepted. No errored call is ever dispatched.
                let permission_mode: JsString = match config.permission.mode {
                    PermissionMode::Default => "default",
                    PermissionMode::Plan => "plan",
                    PermissionMode::BypassPermissions => "bypassPermissions",
                }
                .into();
                let mut results = Vec::with_capacity(source_tool_calls.len());
                for call in &source_tool_calls {
                    let timestamp = clock.now_iso();
                    let missing =
                        crate::ordinary_error_recovery::create_model_interrupted_tool_result(
                            crate::ordinary_error_recovery::ModelInterruptedToolResultInput {
                                call,
                                timestamp: &timestamp,
                                cwd: &config.cwd,
                                permission_mode: &permission_mode,
                            },
                        )
                        .map_err(|error| tool_repair_error(error, call))?;
                    results.push(preparation.project_tool_result(&missing)?);
                }
                let projected = project_tool_results(&results)
                    .map_err(|_| profile_gap("open tool-result projection"))?;
                let mut first = None;
                for mut projected_message in projected {
                    annotate_message(&mut state.timeline, &mut projected_message)?;
                    state.messages.push(projected_message.clone());
                    if first.is_none() {
                        first = Some(projected_message);
                    }
                }
                let Some(first) = first else {
                    return Err(wrong_reply("ordinary model-error tool-result projection"));
                };
                emit_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::ToolResultsProjected { message: first },
                )?;
            }
            // Original AgentLoop.ts:1420-1439: strict `invalid_tool_arguments`
            // equality, at most three corrections per turn, transient prompt
            // then `turn_continued` reason model_error.
            match crate::ordinary_error_recovery::plan_json_self_correction(
                &error,
                config.json_self_correct == Some(true),
                json_self_correct_count,
            )
            .map_err(|projection| tool_repair_error(projection, &error))?
            {
                crate::ordinary_error_recovery::JsonSelfCorrectionPlan::Continue {
                    attempts_used,
                    prompt,
                    purpose,
                } => {
                    json_self_correct_count = attempts_used;
                    push_transient_synthetic_prompt(&mut state, prompt, purpose, clock);
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Continued {
                            reason: TurnContinueReason::ModelError,
                        },
                    )?;
                    continue;
                }
                crate::ordinary_error_recovery::JsonSelfCorrectionPlan::Decline { .. } => {}
            }
            // Original AgentLoop.ts:1444: exactly one probe with the post-projection
            // message list and the real `hasAttemptedCompact` turn state.
            let reactive = probe_ordinary_model_error_recovery(
                host,
                control,
                input,
                &state.messages,
                &error,
                has_attempted_compact,
            )?;
            match reactive {
                // Original AgentLoop.ts:1445-1474. `hasAttemptedOutputRetry` is
                // the same single flag the residual max-output Phase A consumes.
                Some(crate::ports::RecoveryDecision::AdjustOutputAndRetry {
                    max_output_tokens,
                    reason,
                    scope,
                }) if output_recovery.consume_reactive_output_attempt() => {
                    let target = model_error_target(&error, &prepared.route)?;
                    let previous = preparation.current_output_cap(config, &target)?;
                    preparation.set_output_cap(
                        &target,
                        if scope == Some(crate::ports::OutputLimitScope::Attempt) {
                            OutputCapUpdate::Attempt(max_output_tokens)
                        } else {
                            // A:1449-1451: any scope other than "attempt"
                            // (including undefined) is the hard cap.
                            OutputCapUpdate::HardCap(max_output_tokens)
                        },
                    )?;
                    if target != prepared.route {
                        preparation.set_output_cap(
                            &prepared.route,
                            OutputCapUpdate::Attempt(max_output_tokens),
                        )?;
                    }
                    strip_trailing_error_pair(&mut state)?;
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::TokenCapAdjusted {
                            route: target,
                            cap: TokenCapKind::Output,
                            previous,
                            next: max_output_tokens,
                            reason,
                        },
                    )?;
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Continued {
                            reason: TurnContinueReason::ModelError,
                        },
                    )?;
                    continue;
                }
                // Original AgentLoop.ts:1476-1580 compact_and_retry. The guard
                // (`!hasAttemptedCompact`, A:1476) is real: a guard-blocked
                // decision falls through to step 9/10 exactly as Source.
                Some(crate::ports::RecoveryDecision::CompactAndRetry {
                    max_context_tokens,
                    max_output_tokens,
                    reason,
                }) if !has_attempted_compact => {
                    let target = model_error_target(&error, &prepared.route)?;
                    // A:1478: currentMaxContextTokens(target) (A:2570-2578) is
                    // read before any cap is set; it reads the transient
                    // context cap first, which compaction_token_limits mirrors.
                    let previous_context = preparation
                        .compaction_token_limits(config, &target, false)
                        .0;
                    if let Some(next) = max_context_tokens {
                        // A:1479-1492: setTransientTokenCap(target,{maxContextTokens})
                        // (A:2633-2637) then token_cap_adjusted{cap:"context"}.
                        preparation.set_context_cap(&target, next)?;
                        emit_event(
                            events,
                            &mut state.timeline,
                            input,
                            TurnEvent::TokenCapAdjusted {
                                route: target.clone(),
                                cap: TokenCapKind::Context,
                                previous: Some(previous_context),
                                next,
                                reason: reason.clone(),
                            },
                        )?;
                    }
                    if let Some(next) = max_output_tokens {
                        // A:1493-1510: previous read before the set; attempt cap
                        // on the target, plus on the decision when different;
                        // the output event follows the context event.
                        let previous_output = preparation.current_output_cap(config, &target)?;
                        preparation.set_output_cap(&target, OutputCapUpdate::Attempt(next))?;
                        if target != prepared.route {
                            preparation
                                .set_output_cap(&prepared.route, OutputCapUpdate::Attempt(next))?;
                        }
                        emit_event(
                            events,
                            &mut state.timeline,
                            input,
                            TurnEvent::TokenCapAdjusted {
                                route: target.clone(),
                                cap: TokenCapKind::Output,
                                previous: previous_output,
                                next,
                                reason: reason.clone(),
                            },
                        )?;
                    }
                    // A:1511.
                    strip_trailing_error_pair(&mut state)?;
                    // A:1512-1571: compaction inside Source's try/catch; only
                    // fatal compaction errors (A:101-128) escape (A:1563).
                    let outcome = match run_recovery_compaction(
                        &mut state,
                        host,
                        preparation,
                        control,
                        config,
                        input,
                        &target,
                    )? {
                        RecoveryCompactionOutcome::Emergency(compact) => {
                            // A:1549-1557: contextOverflowAfterEmergency
                            // (A:508-534) terminal; no turn_continued.
                            match finish_emergency_compaction(
                                &mut state,
                                started_at.clone(),
                                &compact,
                                clock,
                                TerminalTurnPorts {
                                    host,
                                    events,
                                    input,
                                    control,
                                },
                            ) {
                                Ok(output) => return Ok(output),
                                // Still inside the A:1562 catch: a non-fatal
                                // throw after the StopFailure/result phases
                                // falls into the truncate fallback.
                                Err(error) if known_compaction_advisory(&error) => {
                                    RecoveryCompactionOutcome::Truncate
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        other => other,
                    };
                    if matches!(outcome, RecoveryCompactionOutcome::Truncate) {
                        // A:1558-1561 / 1564-1567 / 1568-1571:
                        // truncateHeadKeepRatio(messages, 0.5) (A:3226) and
                        // tokenCalibrationByRoute.clear().
                        state.messages = truncate_head_preserving_checkpoint(&state.messages, 0.5)?;
                        preparation.clear_compaction_calibration();
                    }
                    // A:1572-1579.
                    has_attempted_compact = true;
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Continued {
                            reason: TurnContinueReason::ModelError,
                        },
                    )?;
                    continue;
                }
                // Original AgentLoop.ts:1582-1597: no engine guard; repeats are
                // bounded only by the host decision.
                Some(crate::ports::RecoveryDecision::TruncateHeadAndRetry {
                    keep_ratio, ..
                }) => {
                    strip_trailing_error_pair(&mut state)?;
                    state.messages =
                        truncate_head_preserving_checkpoint(&state.messages, keep_ratio)?;
                    preparation.clear_compaction_calibration();
                    has_attempted_compact = true;
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Continued {
                            reason: TurnContinueReason::ModelError,
                        },
                    )?;
                    continue;
                }
                // Original AgentLoop.ts:1599-1611.
                Some(crate::ports::RecoveryDecision::StripImagesAndRetry { .. })
                    if !has_attempted_image_strip =>
                {
                    has_attempted_image_strip = true;
                    strip_trailing_error_pair(&mut state)?;
                    state.messages = strip_images_from_messages(&state.messages)?;
                    preparation.clear_compaction_calibration();
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Continued {
                            reason: TurnContinueReason::ModelError,
                        },
                    )?;
                    continue;
                }
                // give_up, undefined, probe failure, absent port, or a
                // guard-blocked branch: no mutation, no event (A:1621 onwards).
                _ => {}
            }
            // Original AgentLoop.ts:1621-1671 Phase A/B/C. Source reads
            // currentMaxOutputTokens(decision) only inside Phase A (A:1626).
            let is_max_output_reached = raw_error
                .get("code")
                .and_then(JsValue::as_string)
                .is_some_and(|code| code.equals_str("max_output_reached"));
            let residual_output_cap = if is_max_output_reached && !output_recovery.counters().0 {
                preparation.current_output_cap(config, &prepared.route)?
            } else {
                None
            };
            let residual_plan = output_recovery.select_residual_max_output(
                crate::output_recovery::OutputRecoveryInput {
                    assembled: &source_assembled,
                    assistant_message: &message.boundary,
                    tool_calls: &source_tool_calls,
                    has_repaired_tool_calls: source_assembled
                        .value()
                        .get("hasRepairedToolCalls")
                        .is_some_and(wire_truthy),
                    original_request: &recovery_request,
                    routed_route: &prepared.route,
                    current_max_output_tokens: residual_output_cap,
                    routed_model_max_output_tokens: prepared
                        .max_output_tokens
                        .or_else(|| preparation.model_output_limit(&prepared.route)),
                },
            )?;
            match apply_pre_emission_output_plan(
                &residual_plan,
                &mut state,
                &started_at,
                config,
                preparation,
                &large_file_repair,
                clock,
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            )? {
                OutputPlanProgress::FallThrough => {}
                OutputPlanProgress::Continue => continue,
                OutputPlanProgress::Completed(output) => return Ok(*output),
            }
            let (stop_reason, classified) = classify_ordinary_model_error(&error)?;
            return finish_ordinary_model_error(
                &mut state,
                started_at,
                &error,
                classified,
                stop_reason,
                crate::create_model_request_failure_status,
                clock,
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            );
        }
        if source_tool_calls.is_empty() {
            // Visible length recovery is AFTER durable assistant publication;
            // it keeps that valid text and shares the pre-emission counters.
            if is_output_length {
                let plan = output_recovery.select_visible_length(
                    crate::output_recovery::OutputRecoveryInput {
                        assembled: &source_assembled,
                        assistant_message: &message.boundary,
                        tool_calls: &source_tool_calls,
                        has_repaired_tool_calls: false,
                        original_request: &recovery_request,
                        routed_route: &prepared.route,
                        current_max_output_tokens: None,
                        routed_model_max_output_tokens: None,
                    },
                )?;
                if matches!(
                    plan.action,
                    crate::output_recovery::OutputRecoveryAction::VisibleLengthExhausted
                ) {
                    emit_output_plan_status(
                        plan.terminal_status(None)?,
                        &mut state,
                        events,
                        input,
                    )?;
                } else {
                    match apply_pre_emission_output_plan(
                        &plan,
                        &mut state,
                        &started_at,
                        config,
                        preparation,
                        &large_file_repair,
                        clock,
                        TerminalTurnPorts {
                            host,
                            events,
                            input,
                            control,
                        },
                    )? {
                        OutputPlanProgress::Continue => continue,
                        OutputPlanProgress::Completed(output) => return Ok(*output),
                        OutputPlanProgress::FallThrough => {}
                    }
                }
            }
            if let Some(decision) = large_file_repair.on_no_tool_calls() {
                let plan = large_file_decision_plan(
                    decision,
                    true,
                    &recovery_request,
                    &prepared.route,
                    &source_assembled,
                );
                match apply_pre_emission_output_plan(
                    &plan,
                    &mut state,
                    &started_at,
                    config,
                    preparation,
                    &large_file_repair,
                    clock,
                    TerminalTurnPorts {
                        host,
                        events,
                        input,
                        control,
                    },
                )? {
                    OutputPlanProgress::Continue => continue,
                    OutputPlanProgress::Completed(output) => return Ok(*output),
                    OutputPlanProgress::FallThrough => {
                        return Err(wrong_reply("large-file no-call decision"));
                    }
                }
            }
            let can_steer = input
                .max_turns
                .is_none_or(|max| max == 0.0 || state.turn_count < max);
            if can_steer && host.advertised("turn", "drain_or_close_steer") {
                let HostReply::SteerDrain(drain) =
                    host.call(control, HostCall::TurnDrainOrClose)?
                else {
                    return Err(wrong_reply("drain_or_close_steer"));
                };
                if !drain.messages.is_empty() {
                    for mut steer in drain.messages {
                        control.check()?;
                        annotate_message(&mut state.timeline, &mut steer.message)?;
                        state.messages.push(steer.message.clone());
                        emit_event(
                            events,
                            &mut state.timeline,
                            input,
                            TurnEvent::SteerApplied {
                                item_id: steer.item_id,
                                message: steer.message,
                            },
                        )?;
                    }
                    state.turn_count += 1.0;
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Continued {
                            reason: TurnContinueReason::UserSteer,
                        },
                    )?;
                    continue;
                }
            }
            let stop = if host.advertised("lifecycle", "dispatch") {
                let mut payload = JsObject::new();
                payload.insert("stopHookActive".into(), JsValue::Bool(false));
                payload.insert(
                    "lastAssistantMessage".into(),
                    JsValue::String(message_text(message.boundary.value())?),
                );
                let HostReply::Lifecycle(result) = host.call(
                    control,
                    HostCall::DispatchLifecycle {
                        event: "Stop".into(),
                        payload: Some(payload),
                    },
                )?
                else {
                    return Err(wrong_reply("Stop lifecycle"));
                };
                Some(result)
            } else {
                None
            };
            emit_event(events, &mut state.timeline, input, TurnEvent::StopRequested)?;
            if let Some(stop) = stop {
                // Source Stop appends hook messages after its public stop event;
                // these messages are not independent assistant-message events.
                state
                    .messages
                    .extend(stop.view.messages.iter().map(|boundary| ProjectedMessage {
                        boundary: boundary.clone(),
                        normalized: MessageProjection::BoundaryOnly(vec![
                            crate::ProjectionGap::OtherBoundaryShape,
                        ]),
                    }));
                if let Some(reason) = stop_block(&stop.boundary)? {
                    let mut error = JsObject::new();
                    error.insert("code".into(), "agent_unsupported_feature".into());
                    error.insert("message".into(), reason.clone());
                    error.insert("details".into(), JsValue::Undefined);
                    let error = BoundarySnapshot::capture(&JsValue::Object(error));
                    let mut result = make_result(
                        &state,
                        started_at,
                        clock.now_iso(),
                        TurnResultKind::Error,
                        TurnStopReason::ToolError,
                    );
                    result.errors.push(error.clone());
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::AgentStatus {
                            event: "lifecycle_blocked".into(),
                            detail: lifecycle_blocked_status(reason),
                        },
                    )?;
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Failed { error },
                    )?;
                    capture_turn_advisory(host, control, input, &state.messages, true)?;
                    emit_event(
                        events,
                        &mut state.timeline,
                        input,
                        TurnEvent::Completed {
                            result: result.clone(),
                        },
                    )?;
                    return Ok(TurnOutput {
                        result,
                        messages: state.messages,
                    });
                }
            }
            if source_assembled
                .value()
                .get("finishReason")
                .and_then(JsValue::as_string)
                .is_some_and(|reason| reason.equals_str("content_filter"))
                && message_text(message.boundary.value())?
                    .units()
                    .iter()
                    .any(|unit| !source_whitespace(*unit))
            {
                emit_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::AgentStatus {
                        event: "content_filter_stop".into(),
                        detail: content_filter_status(),
                    },
                )?;
            }
            let result = make_result(
                &state,
                started_at,
                clock.now_iso(),
                TurnResultKind::Success,
                TurnStopReason::Completed,
            );
            capture_turn_advisory(host, control, input, &state.messages, false)?;
            emit_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::Completed {
                    result: result.clone(),
                },
            )?;
            return Ok(TurnOutput {
                result,
                messages: state.messages,
            });
        }
        if !host.advertised("capability", "execute_batch") {
            return Err(profile_gap(
                "single-tool dispatch/permission fallback profile",
            ));
        }
        let admitted_tool_calls = preparation.project_tool_calls(&source_tool_calls)?;
        emit_event(
            events,
            &mut state.timeline,
            input,
            TurnEvent::ToolCallsDetected {
                calls: admitted_tool_calls.clone(),
                boundaries: source_tool_calls,
            },
        )?;
        control.check()?;
        tool_hints.permission_mode = config.permission.mode;
        // Source creates a fresh context for every batch: a previous truncated
        // response must not mark a later complete invocation as truncated.
        let mut context_extensions = JsObject::new();
        for (key, value) in tool_hints.context_extensions.entries() {
            if !key.equals_str("outputTruncated") {
                context_extensions.insert(key.clone(), value.clone());
            }
        }
        tool_hints.context_extensions = context_extensions;
        if is_output_length || assembled.has_repaired_tool_calls == Some(true) {
            tool_hints
                .context_extensions
                .insert("outputTruncated".into(), JsValue::Bool(true));
        }
        let calls = admitted_tool_calls
            .iter()
            .map(|call| ToolInvocation {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.input.clone(),
            })
            .collect();
        let batch = match host.call(
            control,
            HostCall::ExecuteBatch {
                calls,
                hints: tool_hints.clone(),
            },
        ) {
            Ok(batch) => batch,
            Err(error) if is_deadline_unknown(&error, control) => {
                let _ = finish_aborted_after_tools(
                    state,
                    started_at,
                    clock,
                    TerminalTurnPorts {
                        host,
                        events,
                        input,
                        control,
                    },
                    true,
                );
                return Err(error);
            }
            Err(error) => {
                return Err(capture_after_unknown_abort(
                    host,
                    control,
                    input,
                    &state.messages,
                    error,
                ));
            }
        };
        let (results, notify_observer) = match batch {
            HostReply::ToolResults(results) => (results, true),
            HostReply::ToolBatchRejected { message, .. } => {
                // The completed response already settled this exact batch.
                // Source catches its stock consumer exception; no tool replay.
                let mut results = Vec::with_capacity(admitted_tool_calls.len());
                for call in &admitted_tool_calls {
                    let missing = missing_tool_result(
                        call,
                        &message,
                        &config.cwd,
                        config.permission.mode,
                        clock,
                    )?;
                    results.push(preparation.project_tool_result(&missing)?);
                }
                (results, false)
            }
            _ => return Err(profile_gap("open tool-result consumer profile")),
        };
        // Source's optional observer belongs inside the tool execution pump:
        // a resolved batch invokes it before the outer abort check. Its known
        // ordinary exception replaces this batch's results without re-execution.
        // The stock server applies its global execution abort signal even though
        // the observer port omits a per-call signal. An accepted observer still
        // reconciles its exact identity; cancellation cannot erase Unknown.
        let results = if notify_observer {
            match preparation.observe_tool_results(host, control, input, &results) {
                Ok(()) => results,
                Err(PortError::Cancelled {
                    cause: cause @ (CancelCause::Requested(_) | CancelCause::Deadline),
                }) => {
                    // The stock ordinary catch constructs missing results before
                    // its abort check. Preserve that clock/advice/exception phase
                    // even though none of these results will be published.
                    for call in &admitted_tool_calls {
                        missing_tool_result(
                            call,
                            &"Sidecar module call aborted.".into(),
                            &config.cwd,
                            config.permission.mode,
                            clock,
                        )?;
                    }
                    // Accepted pending calls return AcceptedUnknown or their
                    // correlated ACK; this shortcut never erases responsibility.
                    return finish_aborted_after_tools(
                        state,
                        started_at,
                        clock,
                        TerminalTurnPorts {
                            host,
                            events,
                            input,
                            control: cleanup,
                        },
                        matches!(cause, CancelCause::Deadline),
                    );
                }
                Err(error) if is_deadline_unknown(&error, control) => {
                    let _ = finish_aborted_after_tools(
                        state,
                        started_at,
                        clock,
                        TerminalTurnPorts {
                            host,
                            events,
                            input,
                            control,
                        },
                        true,
                    );
                    return Err(error);
                }
                Err(error) => {
                    let Some(message) = known_tool_observer_exception(&error) else {
                        return Err(capture_after_unknown_abort(
                            host,
                            control,
                            input,
                            &state.messages,
                            error,
                        ));
                    };
                    let mut missing = Vec::with_capacity(admitted_tool_calls.len());
                    for call in &admitted_tool_calls {
                        let boundary = missing_tool_result(
                            call,
                            &message,
                            &config.cwd,
                            config.permission.mode,
                            clock,
                        )?;
                        missing.push(preparation.project_tool_result(&boundary)?);
                    }
                    missing
                }
            }
        } else {
            results
        };
        // A correlated ACK settles the accepted batch even when cancellation
        // arrived first. Source checks abort after the pump/ordinary catch,
        // before publishing these results. Unknown and retirement never recover.
        match control.check() {
            Err(PortError::Cancelled {
                cause: cause @ (CancelCause::Requested(_) | CancelCause::Deadline),
            }) => {
                return finish_aborted_after_tools(
                    state,
                    started_at,
                    clock,
                    TerminalTurnPorts {
                        host,
                        events,
                        input,
                        control: cleanup,
                    },
                    matches!(cause, CancelCause::Deadline),
                );
            }
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        control.check()?;
        // Batch transport checked every original call ID before admission. Missing
        // results, repeated failure annotations, Plan/Todo observers and repair
        // must be integrated before claiming this partial canonical profile whole.
        if results.len() != admitted_tool_calls.len() {
            return Err(profile_gap("missing-result recovery pairing"));
        }
        let paired_boundaries = results
            .iter()
            .map(|r| r.boundary.clone())
            .collect::<Vec<_>>();
        let paired_boundaries = tool_failure_recovery.annotate_results(&paired_boundaries)?;
        let results = paired_boundaries
            .iter()
            .map(|boundary| preparation.project_tool_result(boundary))
            .collect::<PortResult<Vec<_>>>()?;
        let tool_result_repair = large_file_repair
            .analyze_boundary_results(
                &paired_boundaries,
                crate::LargeFileRepairToolContext {
                    output_truncated: is_output_length
                        || assembled.has_repaired_tool_calls == Some(true),
                    repaired_tool_calls: assembled.has_repaired_tool_calls == Some(true),
                    finish_reason: is_output_length
                        .then_some(pilotdeck_protocol::model::FinishReason::Length),
                },
            )
            .map_err(|error| tool_repair_error(error, &source_assembled))?;
        for result in &results {
            if let ToolResultDisposition::Error { code, .. } = &result.disposition
                && (code.equals_str("permission_denied")
                    || code.equals_str("permission_required")
                    || code.equals_str("permission_cancelled"))
            {
                state.permission_denials.push(TurnPermissionDenial {
                    tool_name: result.tool_name.clone(),
                    tool_call_id: result.tool_call_id.clone(),
                    error_code: Some(code.clone()),
                });
            }
            if let ToolResultDisposition::Success { data } = &result.disposition
                && result
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("structuredOutput"))
                    .is_some_and(wire_truthy)
            {
                state.structured_output = data.as_ref().map(BoundarySnapshot::capture);
            }
            if let ToolResultDisposition::Success { data: Some(data) } = &result.disposition
                && let Some(mode) = data
                    .get("requestedMode")
                    .and_then(JsValue::as_string)
                    .and_then(permission_mode)
            {
                let mut effective = mode;
                if mode == PermissionMode::Plan && config.permission.mode != PermissionMode::Plan {
                    config.permission_mode_before_plan = Some(config.permission.mode);
                } else if config.permission.mode == PermissionMode::Plan
                    && mode != PermissionMode::Plan
                    && let Some(previous) = config.permission_mode_before_plan.take()
                {
                    effective = previous;
                }
                config.permission.mode = effective;
                tool_hints.permission_mode = effective;
                emit_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::ModeChangeRequested { mode: effective },
                )?;
            }
            emit_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::ToolResult {
                    result: result.clone(),
                },
            )?;
        }
        let projected = project_tool_results(&results)
            .map_err(|_| profile_gap("open tool-result projection"))?;
        let applied = crate::tool_context::OriginalToolContextPolicy.apply(
            host,
            crate::tool_context::ToolContextInput {
                control,
                input,
                messages: &state.messages,
                results: &results,
                projected: &projected,
            },
        )?;
        let crate::tool_context::ContextMessageCollection::Items(messages) = applied.messages
        else {
            return Err(profile_gap("acknowledged open context history consumer"));
        };
        // Consume the acknowledgement before another cancellation observation.
        // Host context has already accepted responsibility; it is never replayed.
        state.messages = messages;
        let crate::tool_context::ContextMessageCollection::Items(appended) =
            applied.appended_messages
        else {
            return Err(profile_gap("open appended context iteration consumer"));
        };
        let linked_tail = if applied.reattach_tail || applied.fallback_projection_tail {
            Some(
                state
                    .messages
                    .len()
                    .checked_sub(appended.len())
                    .ok_or_else(|| wrong_reply("canonical context appended tail"))?,
            )
        } else {
            None
        };
        for (index, mut message) in appended.into_iter().enumerate() {
            annotate_message(&mut state.timeline, &mut message)?;
            if let Some(start) = linked_tail {
                state.messages[start + index] = message.clone();
            }
            emit_accepted_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::ToolResultsProjected {
                    message: message.clone(),
                },
            )?;
        }
        // Repair decisions are selected before publishing results but applied
        // only after every projected message has received its durable ACK.
        if let Some(decision) = tool_result_repair {
            let plan = large_file_decision_plan(
                decision,
                true,
                &recovery_request,
                &prepared.route,
                &source_assembled,
            );
            match apply_pre_emission_output_plan(
                &plan,
                &mut state,
                &started_at,
                config,
                preparation,
                &large_file_repair,
                clock,
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            )? {
                OutputPlanProgress::Continue => continue,
                OutputPlanProgress::Completed(output) => return Ok(*output),
                OutputPlanProgress::FallThrough => {
                    return Err(wrong_reply("large-file tool-result decision"));
                }
            }
        }
        if let Some(reason) = tool_lifecycle_block(&paired_boundaries) {
            let mut raw = JsObject::new();
            raw.insert("code".into(), "agent_unsupported_feature".into());
            raw.insert("message".into(), JsValue::String(reason.clone()));
            raw.insert("details".into(), JsValue::Undefined);
            let error = BoundarySnapshot::capture(&JsValue::Object(raw));
            let mut result = make_result(
                &state,
                started_at,
                clock.now_iso(),
                TurnResultKind::Error,
                TurnStopReason::ToolError,
            );
            result.errors.push(error.clone());
            let detail = lifecycle_blocked_status(JsValue::String(reason));
            let mut detail = detail
                .value()
                .as_object()
                .expect("constructed lifecycle status")
                .clone();
            detail.insert("stage".into(), "tool_lifecycle".into());
            emit_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::AgentStatus {
                    event: "lifecycle_blocked".into(),
                    detail: BoundarySnapshot::capture(&JsValue::Object(detail)),
                },
            )?;
            return finish_failed_tool_terminal(
                state,
                result,
                error,
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            );
        }
        if crate::tool_failures::ToolFailureRecovery::all_invalid(&paired_boundaries)?
            && large_file_repair.has_pending_repair()
            && let Some(decision) = large_file_repair.on_invalid_tool_input()
        {
            let plan = large_file_decision_plan(
                decision,
                true,
                &recovery_request,
                &prepared.route,
                &source_assembled,
            );
            match apply_pre_emission_output_plan(
                &plan,
                &mut state,
                &started_at,
                config,
                preparation,
                &large_file_repair,
                clock,
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            )? {
                OutputPlanProgress::Continue => continue,
                OutputPlanProgress::Completed(output) => return Ok(*output),
                OutputPlanProgress::FallThrough => {
                    return Err(wrong_reply("large-file invalid-input decision"));
                }
            }
        }
        let invalid_plan = tool_failure_recovery.select_invalid(&paired_boundaries)?;
        match &invalid_plan.action {
            crate::tool_failures::InvalidToolAction::FallThrough {
                reset_output_recovery,
            } => {
                if *reset_output_recovery {
                    // Original AgentLoop.ts:2185-2189 resets the output
                    // counters and `hasAttemptedImageStrip` in the same branch.
                    output_recovery = crate::output_recovery::OutputRecovery::new();
                    has_attempted_image_strip = false;
                }
            }
            crate::tool_failures::InvalidToolAction::Continue { prompt, purpose } => {
                push_transient_synthetic_prompt(&mut state, prompt.clone(), purpose.clone(), clock);
                emit_event(
                    events,
                    &mut state.timeline,
                    input,
                    TurnEvent::Continued {
                        reason: TurnContinueReason::ModelError,
                    },
                )?;
                continue;
            }
            crate::tool_failures::InvalidToolAction::Stop { .. } => {
                let error = invalid_plan.terminal_error()?;
                let mut result = make_result(
                    &state,
                    started_at,
                    clock.now_iso(),
                    TurnResultKind::Error,
                    TurnStopReason::ToolError,
                );
                result.errors.push(error.clone());
                let detail = invalid_plan.terminal_status(&error)?;
                emit_output_plan_status(detail, &mut state, events, input)?;
                return finish_failed_tool_terminal(
                    state,
                    result,
                    error,
                    TerminalTurnPorts {
                        host,
                        events,
                        input,
                        control,
                    },
                );
            }
        }
        if config.stop_on_structured_output == Some(true) && state.structured_output.is_some() {
            let result = make_result(
                &state,
                started_at,
                clock.now_iso(),
                TurnResultKind::Success,
                TurnStopReason::Completed,
            );
            return finish_tool_terminal(
                state,
                result,
                "structured_output_completed".into(),
                structured_output_status(),
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            );
        }
        state.turn_count += 1.0;
        if input
            .max_turns
            .is_some_and(|max| max != 0.0 && state.turn_count > max)
        {
            let max = input.max_turns.expect("predicate requires a maximum");
            let label = if max == f64::NEG_INFINITY {
                "-Infinity".to_owned()
            } else {
                pilotdeck_protocol::stringify_json(&JsValue::Number(max))
                    .map_err(|_| wrong_reply("maximum turns number text"))?
                    .expect("number has a serialization")
            };
            let hint = "Max turn limit reached. Increase maxTurns in config or break the task into smaller steps.";
            let mut error = JsObject::new();
            error.insert("code".into(), "agent_max_turns_reached".into());
            error.insert(
                "message".into(),
                JsValue::String(format!("Reached maximum number of turns ({label}).").into()),
            );
            error.insert("details".into(), JsValue::Undefined);
            error.insert("userHint".into(), hint.into());
            let mut result = make_result(
                &state,
                started_at,
                clock.now_iso(),
                TurnResultKind::MaxTurns,
                TurnStopReason::MaxTurns,
            );
            result
                .errors
                .push(BoundarySnapshot::capture(&JsValue::Object(error)));
            return finish_tool_terminal(
                state,
                result,
                "max_turns_reached".into(),
                max_turns_status(max, &label, hint),
                TerminalTurnPorts {
                    host,
                    events,
                    input,
                    control,
                },
            );
        }
        emit_event(
            events,
            &mut state.timeline,
            input,
            TurnEvent::Continued {
                reason: TurnContinueReason::ToolResults,
            },
        )?;
    }
}
#[allow(clippy::too_many_arguments)]
fn run_routed_compaction(
    state: &mut TurnExecutionState,
    host: &mut dyn HostPort,
    preparation: &mut dyn TurnPreparationPort,
    events: &mut dyn TurnEventPort,
    input: &TurnInput,
    control: &CallControl,
    config: &TurnRuntimeConfig,
    prepared: &mut crate::ports::PreparedInvocation,
    base_request: &CapturedRequest,
) -> Result<CompactionStep, RoutedCompactionFailure> {
    use RoutedCompactionFailure::{Advisory, Fatal};
    let intent = preparation
        .prepare_routed_compaction(
            host,
            control,
            RoutedCompactionPreparation {
                config,
                input,
                messages: &state.messages,
                prepared,
                base_request,
            },
        )
        .map_err(Advisory)?;
    let Some(intent) = intent else {
        return Ok(CompactionStep::Snapshot(None));
    };
    let compact = match host
        .call(control, HostCall::AutoCompact(intent))
        .map_err(Advisory)?
    {
        HostReply::Compacted(compact) => compact.boundary,
        HostReply::CompactedBoundaryOnly(compact) => compact,
        _ => return Err(Fatal(wrong_reply("routed try_auto_compact"))),
    };
    if compact
        .value()
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("compacted"))
    {
        let plan = crate::compaction_persistence::OriginalCompactionPersistencePolicy
            .evaluate(crate::compaction_persistence::CompactionPersistenceInput {
                compact: &compact,
                persistence_advertised: host.advertised("turn", "persist_compaction"),
            })
            .map_err(Advisory)?;
        state.messages = match plan.replacement {
            crate::tool_context::ContextMessageCollection::Items(messages) => messages,
            crate::tool_context::ContextMessageCollection::BoundaryOnly(_) => {
                return Err(Fatal(profile_gap(
                    "open routed compaction replacement collection iteration",
                )));
            }
        };
        preparation.clear_compaction_calibration();
        // Source replaces the canonical window and invalidates calibration
        // before rebuilding. Rebuild/materialization precede persistence.
        prepared.request = preparation
            .rebuild_routed_request(host, control, config, input, &state.messages, prepared)
            .map_err(Advisory)?;
        let commit = match plan.commit {
            Ok(commit) => commit,
            Err(crate::compaction_persistence::CompactionSnapshotError::BoundaryConstruction(
                error,
            )) => return Err(Advisory(error)),
            Err(crate::compaction_persistence::CompactionSnapshotError::PersistencePayload(
                error,
            )) => return Err(Fatal(error)),
        };
        if let Some(commit) = commit {
            let HostReply::CompactionCommitted(_) = host
                .call(
                    control,
                    HostCall::TurnPersistCompaction {
                        boundary: annotate_compaction_boundary(
                            &mut state.timeline,
                            commit.boundary,
                        )
                        .map_err(Fatal)?,
                        messages: commit.messages,
                    },
                )
                .map_err(Fatal)?
            else {
                return Err(Fatal(wrong_reply("routed persist_compaction")));
            };
            control.check().map_err(Fatal)?;
        }
        emit_event(
            events,
            &mut state.timeline,
            input,
            TurnEvent::Continued {
                reason: TurnContinueReason::AutoCompact,
            },
        )
        .map_err(Fatal)?;
        if compact.value().get("error").is_some_and(wire_truthy) {
            emit_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::ContextBudget {
                    snapshot: BoundarySnapshot::capture(
                        compact
                            .value()
                            .get("snapshot")
                            .unwrap_or(&JsValue::Undefined),
                    ),
                },
            )
            .map_err(Fatal)?;
            return Ok(CompactionStep::Emergency(compact));
        }
    }
    // Routed Source announces its snapshot even if absent, unlike the pending
    // pre-route truthy check. No provider stream is advanced before commit ACK.
    Ok(CompactionStep::Snapshot(Some(BoundarySnapshot::capture(
        compact
            .value()
            .get("snapshot")
            .unwrap_or(&JsValue::Undefined),
    ))))
}
enum CompactionStep {
    Snapshot(Option<BoundarySnapshot>),
    Emergency(BoundarySnapshot),
}
enum RoutedCompactionFailure {
    Advisory(PortError),
    Fatal(PortError),
}
#[allow(clippy::too_many_arguments)]
fn install_pre_route_compaction(
    compact: &BoundarySnapshot,
    state: &mut TurnExecutionState,
    host: &mut dyn HostPort,
    preparation: &mut dyn TurnPreparationPort,
    events: &mut dyn TurnEventPort,
    input: &TurnInput,
    control: &CallControl,
) -> PortResult<CompactionStep> {
    if is_compacted_reply(compact) {
        if !install_compaction_replacement(compact, state, host, preparation, control)? {
            return Ok(CompactionStep::Snapshot(None));
        }
        emit_event(
            events,
            &mut state.timeline,
            input,
            TurnEvent::Continued {
                reason: TurnContinueReason::AutoCompact,
            },
        )?;
        if compact.value().get("error").is_some_and(wire_truthy) {
            return Ok(CompactionStep::Emergency(compact.clone()));
        }
    }
    Ok(CompactionStep::Snapshot(
        compact
            .value()
            .get("snapshot")
            .filter(|value| wire_truthy(value))
            .map(BoundarySnapshot::capture),
    ))
}
fn is_compacted_reply(compact: &BoundarySnapshot) -> bool {
    compact
        .value()
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("compacted"))
}
/// Shared Original `compacted` installation (pre-route and A:1545-1547):
/// `messages = compact.messages`, `tokenCalibrationByRoute.clear()`, then
/// `persistCompactSnapshot` (A:2662-2706). Returns `false` when boundary
/// construction (A:2669-2696) threw a non-fatal error that the caller's
/// compaction catch swallows; persistence failures (A:2697-2704
/// CompactionPersistenceError, A:101-128 fatal) propagate.
fn install_compaction_replacement(
    compact: &BoundarySnapshot,
    state: &mut TurnExecutionState,
    host: &mut dyn HostPort,
    preparation: &mut dyn TurnPreparationPort,
    control: &CallControl,
) -> PortResult<bool> {
    let plan = crate::compaction_persistence::OriginalCompactionPersistencePolicy.evaluate(
        crate::compaction_persistence::CompactionPersistenceInput {
            compact,
            persistence_advertised: host.advertised("turn", "persist_compaction"),
        },
    )?;
    state.messages = match plan.replacement {
        crate::tool_context::ContextMessageCollection::Items(messages) => messages,
        crate::tool_context::ContextMessageCollection::BoundaryOnly(_) => {
            return Err(profile_gap(
                "open compaction replacement collection iteration",
            ));
        }
    };
    preparation.clear_compaction_calibration();
    let commit = match plan.commit {
        Ok(commit) => commit,
        Err(crate::compaction_persistence::CompactionSnapshotError::BoundaryConstruction(
            error,
        )) if known_compaction_advisory(&error) => {
            return Ok(false);
        }
        Err(
            crate::compaction_persistence::CompactionSnapshotError::BoundaryConstruction(error)
            | crate::compaction_persistence::CompactionSnapshotError::PersistencePayload(error),
        ) => {
            return Err(error);
        }
    };
    if let Some(commit) = commit {
        // All persistence failures are fatal. An accepted unknown keeps its
        // original effect identity; never enter the advisory compact catch.
        let HostReply::CompactionCommitted(_) = host.call(
            control,
            HostCall::TurnPersistCompaction {
                boundary: annotate_compaction_boundary(&mut state.timeline, commit.boundary)?,
                messages: commit.messages,
            },
        )?
        else {
            return Err(wrong_reply("persist_compaction"));
        };
        control.check()?;
    }
    Ok(true)
}
enum RecoveryCompactionOutcome {
    /// A:1545-1548: replacement installed and persisted, no `compact.error`.
    Replaced,
    /// A:1549-1557: `compact.error` truthy after persistence.
    Emergency(BoundarySnapshot),
    /// A:1558-1571: not compacted, non-fatal throw, or no `tryAutoCompact`.
    Truncate,
}
/// Original AgentLoop.ts:1512-1571 recovery compaction. Unlike the pre-route
/// path (A:654-659) no `turn_continued{auto_compact}` is emitted; the branch
/// yields only its cap events and one final `turn_continued{model_error}`.
/// `awaitCompactionWithEvents` (A:2860-2874) drains host-buffered compaction
/// events; in this architecture those are host-side and pass through directly.
fn run_recovery_compaction(
    state: &mut TurnExecutionState,
    host: &mut dyn HostPort,
    preparation: &mut dyn TurnPreparationPort,
    control: &CallControl,
    config: &TurnRuntimeConfig,
    input: &TurnInput,
    target: &Route,
) -> PortResult<RecoveryCompactionOutcome> {
    use RecoveryCompactionOutcome::{Emergency, Replaced, Truncate};
    // A:1512 `ctx?.tryAutoCompact` absent -> A:1568-1571.
    if !host.advertised("context", "try_auto_compact") {
        return Ok(Truncate);
    }
    // A:1514-1544: limits read after the caps were set; createBudgetRequest
    // (A:2473-2502) runs inside the try, so its non-fatal throw is caught.
    let intent = match preparation.prepare_recovery_compaction(
        host,
        control,
        RecoveryCompactionPreparation {
            config,
            input,
            messages: &state.messages,
            target,
        },
    ) {
        Ok(Some(intent)) => intent,
        Ok(None) => return Ok(Truncate),
        Err(error) if known_compaction_advisory(&error) => return Ok(Truncate),
        Err(error) => return Err(error),
    };
    let compact = match host.call(control, HostCall::AutoCompact(intent)) {
        Ok(HostReply::Compacted(compact)) => compact.boundary,
        Ok(HostReply::CompactedBoundaryOnly(compact)) => compact,
        Ok(_) => return Err(wrong_reply("try_auto_compact")),
        // A:1562-1567: non-fatal compaction failure -> truncate fallback.
        Err(error) if known_compaction_advisory(&error) => return Ok(Truncate),
        // A:1563: fatal compaction errors rethrow.
        Err(error) => return Err(error),
    };
    // A:1558-1561: `compact.type !== "compacted"`.
    if !is_compacted_reply(&compact) {
        return Ok(Truncate);
    }
    // A:1545-1547 (+ persistCompactSnapshot A:1547); a non-fatal boundary
    // construction throw lands in the same A:1562 catch.
    if !install_compaction_replacement(&compact, state, host, preparation, control)? {
        return Ok(Truncate);
    }
    if compact.value().get("error").is_some_and(wire_truthy) {
        return Ok(Emergency(compact));
    }
    Ok(Replaced)
}
fn wrong_reply(operation: &str) -> PortError {
    PortError::InvalidResponse {
        message: format!("{operation} returned a reply for another operation.").into(),
    }
}
struct TerminalTurnPorts<'a> {
    host: &'a mut dyn HostPort,
    events: &'a mut dyn TurnEventPort,
    input: &'a TurnInput,
    control: &'a CallControl,
}
fn annotate_compaction_boundary(
    timeline: &mut TurnTimeline,
    boundary: crate::ports::CompactBoundary,
) -> PortResult<crate::ports::CompactBoundary> {
    let original = boundary.boundary;
    let mut value = original
        .value()
        .as_object()
        .cloned()
        .ok_or_else(|| wrong_reply("compact callback boundary"))?;
    if value
        .get("kind")
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("compact"))
        && let Some(JsValue::Object(metadata)) = value.get("compactMetadata")
        && let Some(id) = metadata.get("compactionId").filter(|id| wire_truthy(id))
    {
        let id = crate::turn_timeline::prefixed("compact:", Some(id))
            .map_err(|error| timeline_error(error, &original))?;
        let mut metadata = metadata.clone();
        metadata.insert(
            "timeline".into(),
            timeline.position_boundary(id, false).value().clone(),
        );
        value.insert("compactMetadata".into(), JsValue::Object(metadata));
    }
    Ok(crate::ports::CompactBoundary {
        boundary: BoundarySnapshot::capture(&JsValue::Object(value)),
    })
}
#[allow(clippy::too_many_arguments)]
fn finish_pre_model_abort(
    mut state: TurnExecutionState,
    started_at: JsString,
    error: PortError,
    host: &mut dyn HostPort,
    preparation: &mut dyn TurnPreparationPort,
    events: &mut dyn TurnEventPort,
    clock: &dyn TurnClock,
    input: &TurnInput,
    cleanup: &CallControl,
) -> PortResult<TurnOutput> {
    let cause = match error {
        PortError::Cancelled {
            cause: cause @ (CancelCause::Requested(_) | CancelCause::Deadline),
        } => cause,
        // AcceptedUnknown, retirement, transport loss and ordinary failures
        // retain their original classification; no completion is published.
        other => return Err(other),
    };
    // The target protocol strengthens the original fire-and-forget boundary:
    // resolve accepted notifications before announcing or capturing a known
    // cancellation. An unresolved call preserves Unknown and stops this path.
    preparation.finish_pending_effects()?;
    // Source's loop-top and post-createModelRequest abort paths construct the
    // result before status/capture, retaining any prior assistant and unconsumed
    // synthetic prompts. No model request or tool effect follows this result.
    let result = make_result(
        &state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Aborted,
        TurnStopReason::AbortedStreaming,
    );
    let deadline = matches!(cause, CancelCause::Deadline);
    if let CancelCause::Requested(reason) = cause {
        let lower = reason
            .units()
            .iter()
            .map(|unit| {
                if (0x41..=0x5a).contains(unit) {
                    unit + 0x20
                } else {
                    *unit
                }
            })
            .collect::<Vec<_>>();
        if ["timeout", "cancel", "abort"].iter().any(|word| {
            lower
                .windows(word.len())
                .any(|part| part.iter().copied().eq(word.encode_utf16()))
        }) {
            emit_accepted_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::AgentStatus {
                    event: "turn_aborted".into(),
                    detail: abort_status_detail(reason),
                },
            )?;
        }
    }
    let capture_cleanup = fresh_capture_cleanup(cleanup);
    let capture = capture_turn_advisory(host, &capture_cleanup, input, &state.messages, false);
    // An advisory capture may expire before dispatch when the writer stalls.
    // Execution Deadline still publishes completion; hard stops propagate.
    if !matches!(
        &capture,
        Err(PortError::AcceptedUnknown { .. }
            | PortError::Cancelled {
                cause: CancelCause::Requested(_) | CancelCause::Deadline,
            }) if deadline && capture_cleanup.cancellation.hard_stop_cause().is_none()
    ) {
        capture?;
    }
    emit_accepted_event(
        events,
        &mut state.timeline,
        input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}

fn finish_aborted_after_tools(
    mut state: TurnExecutionState,
    started_at: JsString,
    clock: &dyn TurnClock,
    ports: TerminalTurnPorts<'_>,
    deadline: bool,
) -> PortResult<TurnOutput> {
    // AgentLoop's post-execute abort path creates the result before capture,
    // retains the admitted assistant, and publishes no tool results/status.
    let result = make_result(
        &state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Aborted,
        TurnStopReason::AbortedStreaming,
    );
    let capture_cleanup = fresh_capture_cleanup(ports.control);
    let capture = capture_turn_advisory(
        ports.host,
        &capture_cleanup,
        ports.input,
        &state.messages,
        false,
    );
    // An advisory capture may expire before dispatch when the writer stalls.
    // Execution Deadline still publishes completion; hard stops propagate.
    if !matches!(
        &capture,
        Err(PortError::AcceptedUnknown { .. }
            | PortError::Cancelled {
                cause: CancelCause::Requested(_) | CancelCause::Deadline,
            }) if deadline && capture_cleanup.cancellation.hard_stop_cause().is_none()
    ) {
        capture?;
    }
    emit_accepted_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}
// Source collectToolCalls projects fresh own id/name/input/raw entries, rather
// than accepting the assembler's cached call list as the owning-turn oracle.
fn collect_recovery_tool_calls(message: &BoundarySnapshot) -> PortResult<Vec<BoundarySnapshot>> {
    let Some(JsValue::Array(content)) = message.value().get("content") else {
        return Err(profile_gap("open recovery assistant content collection"));
    };
    let mut calls = Vec::new();
    for block in content {
        if matches!(block, JsValue::Null | JsValue::Undefined) {
            let label = if matches!(block, JsValue::Null) {
                "null"
            } else {
                "undefined"
            };
            return Err(PortError::SourceException {
                message: format!("Cannot read properties of {label} (reading 'type')").into(),
                boundary: BoundarySnapshot::capture(block),
            });
        }
        if block
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str("tool_call"))
        {
            let mut call = JsObject::new();
            for key in ["id", "name", "input", "raw"] {
                call.insert(
                    key.into(),
                    block.get(key).cloned().unwrap_or(JsValue::Undefined),
                );
            }
            calls.push(BoundarySnapshot::capture(&JsValue::Object(call)));
        }
    }
    Ok(calls)
}
fn push_transient_synthetic_prompt(
    state: &mut TurnExecutionState,
    prompt: JsString,
    purpose: JsString,
    clock: &dyn TurnClock,
) {
    let id = clock.uuid();
    let mut metadata = JsObject::new();
    metadata.insert("synthetic".into(), JsValue::Bool(true));
    metadata.insert("transient".into(), JsValue::Bool(true));
    metadata.insert("transientId".into(), JsValue::String(id.clone()));
    metadata.insert("purpose".into(), JsValue::String(purpose));
    let mut block = JsObject::new();
    block.insert("type".into(), JsValue::from("text"));
    block.insert("text".into(), JsValue::String(prompt));
    let mut message = JsObject::new();
    message.insert("role".into(), JsValue::from("user"));
    message.insert(
        "content".into(),
        JsValue::Array(vec![JsValue::Object(block)]),
    );
    message.insert("metadata".into(), JsValue::Object(metadata));
    state.messages.push(ProjectedMessage {
        boundary: BoundarySnapshot::capture(&JsValue::Object(message)),
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
    });
    state.transient_prompt_ids.push(id);
}
fn expire_consumed_transient_prompts(state: &mut TurnExecutionState) {
    if state.transient_prompt_ids.is_empty() {
        return;
    }
    state.messages.retain(|message| {
        let value = message.boundary.value();
        let metadata = value.get("metadata");
        let is_active = value
            .get("role")
            .and_then(JsValue::as_string)
            .is_some_and(|role| role.equals_str("user"))
            && metadata.and_then(|v| v.get("transient")) == Some(&JsValue::Bool(true))
            && metadata
                .and_then(|v| v.get("transientId"))
                .and_then(JsValue::as_string)
                .is_some_and(|id| state.transient_prompt_ids.contains(id));
        !is_active
    });
    state.transient_prompt_ids.clear();
}
fn append_recovery_assistant(
    state: &mut TurnExecutionState,
    boundary: BoundarySnapshot,
    events: &mut dyn TurnEventPort,
    input: &TurnInput,
) -> PortResult<()> {
    let mut message = ProjectedMessage {
        boundary,
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
    };
    annotate_message(&mut state.timeline, &mut message)?;
    state.final_message = Some(message.clone());
    state.messages.push(message.clone());
    // Module transport completes bounded FIFO admission here. The stock host
    // consumes the assistant event/callback before dispatching later queued
    // host calls. This does not claim a direct-native durability ACK.
    emit_event(
        events,
        &mut state.timeline,
        input,
        TurnEvent::AssistantMessage { message },
    )
}
fn finish_early_stream_exhausted(
    mut state: TurnExecutionState,
    started_at: JsString,
    plan: crate::early_stream_recovery::EarlyStreamPlan,
    clock: &dyn TurnClock,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    let error = plan.terminal_error()?;
    let partial = plan.partial_text_message()?;
    state.final_message = None;
    if let Some(partial) = partial {
        append_recovery_assistant(&mut state, partial, ports.events, ports.input)?;
    }
    let message = error
        .value()
        .get("message")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| wrong_reply("early recovery terminal error message"))?;
    if ports.host.advertised("lifecycle", "dispatch") {
        let mut payload = JsObject::new();
        payload.insert("error".into(), JsValue::String(message.clone()));
        let HostReply::Lifecycle(_) = ports.host.call(
            ports.control,
            HostCall::DispatchLifecycle {
                event: "StopFailure".into(),
                payload: Some(payload),
            },
        )?
        else {
            return Err(wrong_reply("early recovery StopFailure lifecycle"));
        };
    }
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::StopFailure { error: message },
    )?;
    let mut result = make_result(
        &state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Error,
        TurnStopReason::ModelError,
    );
    result.errors.push(error.clone());
    // Status conversion may genuinely throw after the earlier public partial,
    // StopFailure and result creation. Keep it deferred to the Source phase.
    let status = plan.terminal_status(&error)?;
    let status_event = status
        .value()
        .get("event")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| wrong_reply("early recovery terminal status event"))?;
    let detail =
        BoundarySnapshot::capture(status.value().get("detail").unwrap_or(&JsValue::Undefined));
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::AgentStatus {
            event: status_event,
            detail,
        },
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Failed { error },
    )?;
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        true,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}
fn finish_emergency_compaction(
    state: &mut TurnExecutionState,
    started_at: JsString,
    compact: &BoundarySnapshot,
    clock: &dyn TurnClock,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    let plan = crate::compaction_terminal::OriginalEmergencyCompactionTerminal
        .evaluate(crate::compaction_terminal::EmergencyCompactionTerminalInput { compact })?;
    let message = plan
        .error
        .value()
        .get("message")
        .cloned()
        .ok_or_else(|| wrong_reply("emergency error message"))?;
    if ports.host.advertised("lifecycle", "dispatch") {
        let mut payload = JsObject::new();
        payload.insert("error".into(), message.clone());
        let HostReply::Lifecycle(_) = ports.host.call(
            ports.control,
            HostCall::DispatchLifecycle {
                event: "StopFailure".into(),
                payload: Some(payload),
            },
        )?
        else {
            return Err(wrong_reply("emergency StopFailure lifecycle"));
        };
    }
    // Source creates its result before publishing stop_failure and evaluating
    // the late status. Truthy non-string errors must retain this phase ordering.
    let mut result = make_result(
        state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Error,
        TurnStopReason::PromptTooLong,
    );
    result.errors.push(plan.error.clone());
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::StopFailureBoundary {
            error: BoundarySnapshot::capture(&message),
        },
    )?;
    let detail = plan.status_detail?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::AgentStatus {
            event: "model_request_failed".into(),
            detail,
        },
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Failed { error: plan.error },
    )?;
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        true,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages.clone(),
    })
}
fn finish_plain_model_stream_throw(
    mut state: TurnExecutionState,
    started_at: JsString,
    message: JsString,
    clock: &dyn TurnClock,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    if ports.host.advertised("lifecycle", "dispatch") {
        let mut payload = JsObject::new();
        payload.insert("error".into(), JsValue::String(message.clone()));
        let HostReply::Lifecycle(_) = ports.host.call(
            ports.control,
            HostCall::DispatchLifecycle {
                event: "StopFailure".into(),
                payload: Some(payload),
            },
        )?
        else {
            return Err(wrong_reply("plain stream StopFailure lifecycle"));
        };
    }
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::StopFailure {
            error: message.clone(),
        },
    )?;
    let mut raw_error = JsObject::new();
    raw_error.insert("code".into(), "agent_model_error".into());
    raw_error.insert("message".into(), JsValue::String(message.clone()));
    raw_error.insert("details".into(), JsValue::Undefined);
    let error = BoundarySnapshot::capture(&JsValue::Object(raw_error));
    let mut result = make_result(
        &state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Error,
        TurnStopReason::ModelError,
    );
    // Source iterator catch omits structuredOutput even when an earlier
    // acknowledged tool batch populated the turn-local accumulator.
    result.structured_output = None;
    result.errors.push(error.clone());
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::AgentStatus {
            event: "model_request_failed".into(),
            detail: plain_model_failure_status(&message),
        },
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Failed { error },
    )?;
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        true,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}
/// Source tryReactiveRecover (AgentLoop.ts:2241-2263) catches known ordinary
/// probe exceptions and returns `undefined`; an absent capability makes no
/// call. It never licenses a new call identity after accepted uncertainty or
/// retirement. `None` means "no decision" exactly as the Source `undefined`.
fn probe_ordinary_model_error_recovery(
    host: &mut dyn HostPort,
    control: &CallControl,
    input: &TurnInput,
    messages: &[ProjectedMessage],
    error: &BoundarySnapshot,
    has_attempted_compact: bool,
) -> PortResult<Option<crate::ports::RecoveryDecision>> {
    if !host.advertised("context", "recover_from_model_error") {
        return Ok(None);
    }
    let reply = host.call(
        control,
        HostCall::RecoverModelErrorBoundary(crate::ports::RecoverModelErrorBoundary {
            session_id: input.session_id.clone(),
            turn_id: input.turn_id.clone(),
            error: error.clone(),
            messages: messages.to_vec(),
            has_attempted_compact,
        }),
    );
    match reply {
        Ok(HostReply::Recovery(recovery)) => Ok(Some(recovery.view)),
        Ok(HostReply::RecoveryBoundaryOnly(recovery)) => {
            let kind = recovery.value().get("type").and_then(JsValue::as_string);
            if kind.is_some_and(|kind| {
                [
                    "adjust_output_and_retry",
                    "compact_and_retry",
                    "truncate_head_and_retry",
                    "strip_images_and_retry",
                ]
                .iter()
                .any(|name| kind.equals_str(name))
            }) {
                // A decision shape the typed admission could not accept (for
                // example a missing keepRatio) is a bounded explicit gap, not a
                // silently invented give-up.
                Err(profile_gap(
                    "ordinary model-error open reactive recovery action",
                ))
            } else {
                // Source ignores undefined/null/falsy and future decision kinds.
                Ok(None)
            }
        }
        Err(
            PortError::HostFailure { .. }
            | PortError::KnownConsumerException { .. }
            | PortError::SourceException { .. },
        ) => Ok(None),
        Err(error) => Err(error),
        Ok(_) => Err(wrong_reply("ordinary model-error recovery probe")),
    }
}
/// Original `modelErrorTarget` (AgentLoop.ts:4053-4061): JS `||` falls back
/// for every falsy provider/model (undefined, null, "", 0, false, NaN). A
/// truthy non-string target is not a Route and stays an explicit gap.
fn model_error_target(error: &BoundarySnapshot, decision: &Route) -> PortResult<Route> {
    let pick = |key: &str, fallback: &JsString| -> PortResult<JsString> {
        match error.value().get(key) {
            Some(JsValue::String(value)) if !value.is_empty() => Ok(value.clone()),
            Some(value) if wire_truthy(value) => {
                Err(profile_gap("non-string model error target route"))
            }
            _ => Ok(fallback.clone()),
        }
    };
    Ok(Route {
        provider: pick("provider", &decision.provider)?,
        model: pick("model", &decision.model)?,
    })
}

/// Before the ordinary terminal profile, preserve Source's first regex
/// phase. A matching reasoning retry remains an explicit unsupported profile,
/// rather than silently surfacing an error that Source would retry once.
fn requires_reasoning_content_retry(error: &BoundarySnapshot) -> PortResult<bool> {
    let text =
        model_error_regexp_text(error.value().get("message").unwrap_or(&JsValue::Undefined))?;
    let units: Vec<u16> = text
        .units()
        .iter()
        .map(|&u| if (65..=90).contains(&u) { u + 32 } else { u })
        .collect();
    let word = |u: u16| (97..=122).contains(&u) || (48..=57).contains(&u) || u == 95;
    let reasoning: Vec<u16> = "reasoning_content".encode_utf16().collect();
    let has_reasoning = units
        .windows(reasoning.len())
        .enumerate()
        .any(|(i, value)| {
            value == reasoning
                && (i == 0 || !word(units[i - 1]))
                && (i + reasoning.len() == units.len() || !word(units[i + reasoning.len()]))
        });
    if !has_reasoning {
        return Ok(false);
    }
    let spaced = |prefix: &str, suffix: &str| {
        let prefix: Vec<u16> = prefix.encode_utf16().collect();
        let suffix: Vec<u16> = suffix.encode_utf16().collect();
        units.windows(prefix.len()).enumerate().any(|(i, value)| {
            if value != prefix { return false; }
            let first = i + prefix.len();
            let mut next = first;
            while next < units.len() && matches!(units[next], 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff) { next += 1; }
            next > first && units[next..].starts_with(&suffix)
        })
    };
    Ok(spaced("thinking", "mode") && (spaced("pass", "back") || spaced("passed", "back")))
}

/// Source classifyModelError/isPromptTooLong keep the complete surfaced
/// value as details; neither retryable nor a JSON property grants authority.
fn classify_ordinary_model_error(
    error: &BoundarySnapshot,
) -> PortResult<(TurnStopReason, BoundarySnapshot)> {
    let raw = error.value();
    let undefined = JsValue::Undefined;
    let message = raw.get("message").unwrap_or(&undefined);
    let prompt_too_long = raw
        .get("code")
        .and_then(JsValue::as_string)
        .is_some_and(|code| code.equals_str("prompt_too_long"))
        || raw.get("recoverableViaCompact").is_some_and(wire_truthy)
        || {
            // JS RegExp.test coerces message only after the two short circuits.
            let text = model_error_regexp_text(message)?;
            [
                "prompt is too long",
                "input length and max_tokens exceed context limit",
                "request too large",
            ]
            .iter()
            .any(|pattern| ascii_case_insensitive_contains(&text, pattern))
        };
    let hint = raw.get("userHint").unwrap_or(&undefined);
    let default_hint = JsValue::from(
        "Input exceeds the model context window. Try /compact to compress history or /new for a fresh session.",
    );
    let hint = if prompt_too_long && matches!(hint, JsValue::Null | JsValue::Undefined) {
        &default_hint
    } else {
        hint
    };
    let mut classified = JsObject::new();
    classified.insert(
        "code".into(),
        if prompt_too_long {
            "agent_prompt_too_long".into()
        } else {
            "agent_model_error".into()
        },
    );
    classified.insert("message".into(), message.clone());
    classified.insert("details".into(), raw.clone());
    if wire_truthy(hint) {
        classified.insert("userHint".into(), hint.clone());
    }
    Ok((
        if prompt_too_long {
            TurnStopReason::PromptTooLong
        } else {
            TurnStopReason::ModelError
        },
        BoundarySnapshot::capture(&JsValue::Object(classified)),
    ))
}
fn ascii_case_insensitive_contains(text: &JsString, pattern: &str) -> bool {
    let pattern: Vec<u16> = pattern.encode_utf16().collect();
    text.units().windows(pattern.len()).any(|window| {
        window.iter().zip(&pattern).all(|(&a, &b)| {
            let lower = |unit: u16| {
                if (65..=90).contains(&unit) {
                    unit + 32
                } else {
                    unit
                }
            };
            lower(a) == lower(b)
        })
    })
}
/// JSON-boundary ToString subset used by non-Unicode JS /i RegExp.test.
/// Callable/getter/native object graphs still require the host binding island.
fn model_error_regexp_text(value: &JsValue) -> PortResult<JsString> {
    enum Work<'a> {
        Value(&'a JsValue),
        Comma,
    }
    let mut work = vec![Work::Value(value)];
    let mut units = Vec::new();
    while let Some(item) = work.pop() {
        let value = match item {
            Work::Comma => {
                units.push(44);
                continue;
            }
            Work::Value(value) => value,
        };
        let text: JsString = match value {
            JsValue::Undefined => "undefined".into(),
            JsValue::Null => "null".into(),
            JsValue::Bool(true) => "true".into(),
            JsValue::Bool(false) => "false".into(),
            JsValue::String(text) => text.clone(),
            JsValue::Number(number) if number.is_nan() => "NaN".into(),
            JsValue::Number(number) if *number == f64::INFINITY => "Infinity".into(),
            JsValue::Number(number) if *number == f64::NEG_INFINITY => "-Infinity".into(),
            JsValue::Number(_) => pilotdeck_protocol::stringify_json(value)
                .map_err(|_| profile_gap("model-error primitive number coercion"))?
                .ok_or_else(|| wrong_reply("model-error number coercion"))?
                .into(),
            JsValue::Object(object) => {
                if object.contains_key("toString") {
                    return Err(PortError::SourceException {
                        message: "Cannot convert object to primitive value".into(),
                        boundary: BoundarySnapshot::capture(value),
                    });
                }
                "[object Object]".into()
            }
            JsValue::Array(items) => {
                for (index, item) in items.iter().enumerate().rev() {
                    if !matches!(item, JsValue::Null | JsValue::Undefined) {
                        work.push(Work::Value(item));
                    }
                    if index != 0 {
                        work.push(Work::Comma);
                    }
                }
                continue;
            }
        };
        units.extend_from_slice(text.units());
    }
    Ok(JsString::from_utf16(units))
}

/// Original ordinary model-error terminal phase (AgentLoop1668-1696).
/// The caller must have published/durably acknowledged the assistant and
/// exhausted or declined earlier recovery gates. Status construction is late:
/// Source may throw while building it AFTER StopFailure and the result clock.
#[allow(clippy::too_many_arguments)]
fn finish_ordinary_model_error(
    state: &mut TurnExecutionState,
    started_at: JsString,
    model_error: &BoundarySnapshot,
    classified_error: BoundarySnapshot,
    stop_reason: TurnStopReason,
    build_status: impl FnOnce(&BoundarySnapshot, &BoundarySnapshot) -> PortResult<BoundarySnapshot>,
    clock: &dyn TurnClock,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    if ports.host.advertised("lifecycle", "dispatch") {
        let mut payload = JsObject::new();
        payload.insert("error".into(), model_error.value().clone());
        let HostReply::Lifecycle(_) = ports.host.call(
            ports.control,
            HostCall::DispatchLifecycle {
                event: "StopFailure".into(),
                payload: Some(payload),
            },
        )?
        else {
            return Err(wrong_reply("ordinary model-error StopFailure lifecycle"));
        };
    }
    let failure_text = match model_error.value() {
        JsValue::String(text) => JsValue::String(text.clone()),
        value => pilotdeck_protocol::stringify_json(value)
            .map_err(|_| profile_gap("ordinary model-error JSON serialization"))?
            .map(|text| JsValue::String(text.into()))
            .unwrap_or(JsValue::Undefined),
    };
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::StopFailureBoundary {
            error: BoundarySnapshot::capture(&failure_text),
        },
    )?;
    let mut result = make_result(
        state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Error,
        stop_reason,
    );
    // Source ordinary-error result omits structuredOutput, even when an
    // earlier tool round set it. The turn-local value itself is not erased.
    result.structured_output = None;
    result.errors.push(classified_error.clone());
    let status = build_status(&classified_error, model_error)?;
    let event = status
        .value()
        .get("event")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| wrong_reply("model failure status event"))?;
    let detail = status
        .value()
        .get("detail")
        .map(BoundarySnapshot::capture)
        .ok_or_else(|| wrong_reply("model failure status detail"))?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        // AgentLoop.toAgentStatusEvent excludes the internal status text.
        TurnEvent::AgentStatus { event, detail },
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Failed {
            error: classified_error,
        },
    )?;
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        true,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages.clone(),
    })
}

fn known_tool_observer_exception(error: &PortError) -> Option<JsString> {
    match error {
        PortError::KnownConsumerException { message, .. } => Some(message.clone()),
        // Generic port/admission/Source errors have no proven thrown-object
        // provenance here. Never infer ordinary or terminal from diagnostic JSON.
        _ => None,
    }
}
/// Original createMissingToolResult samples one timestamp before advice,
/// retaining the original error text separately from model-visible content.
fn missing_tool_result(
    call: &ToolCall,
    message: &JsString,
    cwd: &JsString,
    permission_mode: PermissionMode,
    clock: &dyn TurnClock,
) -> PortResult<BoundarySnapshot> {
    fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
        let mut object = JsObject::new();
        for (key, value) in fields {
            object.insert(key.into(), value);
        }
        JsValue::Object(object)
    }
    let timestamp = clock.now_iso();
    let recovery = crate::tool_error_recovery::build_tool_error_recovery(
        &crate::tool_error_recovery::ToolErrorRecoveryInput {
            code: "tool_execution_failed".into(),
            tool_name: call.name.clone(),
            message: message.clone(),
            cwd: cwd.clone(),
            permission_mode: match permission_mode {
                PermissionMode::Default => "default",
                PermissionMode::Plan => "plan",
                PermissionMode::BypassPermissions => "bypassPermissions",
            }
            .into(),
            details: None,
        },
    )
    .map_err(|error| {
        tool_repair_error(
            error,
            &BoundarySnapshot::capture(&JsValue::String(message.clone())),
        )
    })?;
    Ok(BoundarySnapshot::capture(&object([
        ("type", "error".into()),
        ("toolCallId", JsValue::String(call.id.clone())),
        ("toolName", JsValue::String(call.name.clone())),
        (
            "error",
            object([
                ("code", "tool_execution_failed".into()),
                ("message", JsValue::String(message.clone())),
            ]),
        ),
        (
            "content",
            JsValue::Array(vec![object([
                ("type", "text".into()),
                ("text", JsValue::String(recovery.message)),
            ])]),
        ),
        (
            "metadata",
            object([("recovery", recovery.advice.value().clone())]),
        ),
        ("startedAt", JsValue::String(timestamp.clone())),
        ("completedAt", JsValue::String(timestamp)),
    ])))
}
/// Source createModelRequestFailedStatus with modelError undefined only.
/// Provider errors have a separate action/metadata policy and cannot use this.
fn plain_model_failure_status(message: &JsString) -> BoundarySnapshot {
    fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
        let mut result = JsObject::new();
        for (key, value) in fields {
            result.insert(key.into(), value);
        }
        JsValue::Object(result)
    }
    const HINT: &str = "Check Settings → Model Provider and verify the selected provider, base URL, API key, model name, and timeoutMs. If the provider is slow or the network is unstable, increase timeoutMs or check provider status.";
    let provider_message: JsString = if message.is_empty() {
        "The model request failed, so this turn has stopped.".into()
    } else {
        message.clone()
    };
    let units = provider_message.units();
    let first = units
        .iter()
        .position(|u| !source_whitespace(*u))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !source_whitespace(*u))
        .map_or(first, |n| n + 1);
    let mut text = if first == end {
        "The model request failed."
            .encode_utf16()
            .collect::<Vec<_>>()
    } else {
        units[first..end].to_vec()
    };
    text.extend("\n\nAction: ".encode_utf16());
    text.extend(HINT.encode_utf16());
    BoundarySnapshot::capture(&object([
        ("message", JsValue::String(JsString::from_utf16(text))),
        (
            "messageI18n",
            object([
                ("key", "chat:agentStatus.modelRequestFailed.message".into()),
                (
                    "params",
                    object([("providerMessage", JsValue::String(provider_message.clone()))]),
                ),
            ]),
        ),
        ("code", "agent_model_error".into()),
        ("severity", "error".into()),
        ("visible", JsValue::Bool(true)),
        ("userHint", HINT.into()),
        (
            "userHintI18n",
            object([
                (
                    "key",
                    "chat:agentStatus.modelRequestFailed.actions.settingsDefault".into(),
                ),
                ("params", object([])),
            ]),
        ),
        ("scope", "turn".into()),
        ("source", "agent".into()),
        ("providerMessage", JsValue::String(provider_message)),
        ("fixTarget", "settings".into()),
    ]))
}

fn finish_budget_terminal(
    mut state: TurnExecutionState,
    started_at: JsString,
    plan: crate::budget_terminal::BudgetTerminalPlan,
    clock: &dyn TurnClock,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    state.final_message = plan.safe_message;
    if let Some(message) = state.final_message.as_mut() {
        annotate_message(&mut state.timeline, message)?;
        state.messages.push(message.clone());
        emit_event(
            ports.events,
            &mut state.timeline,
            ports.input,
            TurnEvent::AssistantMessage {
                message: message.clone(),
            },
        )?;
    }
    let error_message = plan
        .error
        .value()
        .get("message")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| wrong_reply("budget error message"))?;
    if ports.host.advertised("lifecycle", "dispatch") {
        let mut payload = JsObject::new();
        payload.insert("error".into(), JsValue::String(error_message.clone()));
        let HostReply::Lifecycle(_) = ports.host.call(
            ports.control,
            HostCall::DispatchLifecycle {
                event: "StopFailure".into(),
                payload: Some(payload),
            },
        )?
        else {
            return Err(wrong_reply("StopFailure lifecycle"));
        };
    }
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::StopFailure {
            error: error_message.clone(),
        },
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        // AgentLoop's status projection omits text; the sidecar client falls
        // back to the event name. Keep the diagnostic in StopFailure/errors.
        TurnEvent::AgentStatus {
            event: plan.status_event,
            detail: plan.status_detail,
        },
    )?;
    // Source records completion time after the failure lifecycle and status.
    let mut result = make_result(
        &state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Error,
        plan.stop_reason,
    );
    result.errors.push(plan.error.clone());
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Failed { error: plan.error },
    )?;
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        true,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}
fn tool_repair_error(error: crate::ProjectionError, boundary: &BoundarySnapshot) -> PortError {
    match error {
        crate::ProjectionError::SourceTypeError { message } => PortError::SourceException {
            message,
            boundary: boundary.clone(),
        },
        crate::ProjectionError::InvalidBoundary { message }
        | crate::ProjectionError::Codec { message } => PortError::InvalidResponse { message },
    }
}
fn tool_lifecycle_block(results: &[BoundarySnapshot]) -> Option<JsString> {
    results.iter().find_map(|result| {
        result
            .value()
            .get("metadata")?
            .get("lifecycle")?
            .as_object()?
            .get("blocked")?
            .as_object()?
            .get("reason")?
            .as_string()
            .cloned()
    })
}
fn finish_failed_tool_terminal(
    mut state: TurnExecutionState,
    result: TurnResult,
    error: BoundarySnapshot,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Failed { error },
    )?;
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        true,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}
fn finish_tool_terminal(
    mut state: TurnExecutionState,
    result: TurnResult,
    event: JsString,
    detail: BoundarySnapshot,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::AgentStatus { event, detail },
    )?;
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        result.kind == TurnResultKind::Error,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}
/// Capture is the sole terminal cleanup effect admitted after user abort.
/// Its budget starts here, independently of the expired execution deadline.
fn fresh_capture_cleanup(previous: &CallControl) -> CallControl {
    CallControl {
        cancellation: previous.cancellation.cleanup_scope(),
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
    }
}

// Already received safe text belongs in the memory snapshot even when the
// next pull is unresolved. Do not publish a new durable message after Unknown.
fn safe_unknown_capture_partial(assembled: &BoundarySnapshot) -> Option<ProjectedMessage> {
    if !matches!(assembled.value().get("toolCalls"), Some(JsValue::Array(calls)) if calls.is_empty())
    {
        return None;
    }
    let mut message = assembled.value().get("message")?.as_object()?.clone();
    let JsValue::Array(content) = message.get("content")? else {
        return None;
    };
    let content: Vec<_> = content
        .iter()
        .filter(|block| {
            !block
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("thinking"))
        })
        .cloned()
        .collect();
    if !content.iter().any(|block| {
        block
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str("text"))
            && block
                .get("text")
                .and_then(JsValue::as_string)
                .is_some_and(|text| text.units().iter().any(|unit| !source_whitespace(*unit)))
    }) {
        return None;
    }
    message.insert("content".into(), JsValue::Array(content));
    Some(ProjectedMessage {
        boundary: BoundarySnapshot::capture(&JsValue::Object(message)),
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
    })
}

fn capture_after_unknown_abort(
    host: &mut dyn HostPort,
    execution: &CallControl,
    input: &TurnInput,
    messages: &[ProjectedMessage],
    original: PortError,
) -> PortError {
    if matches!(original, PortError::AcceptedUnknown { .. })
        && matches!(
            execution.check(),
            Err(PortError::Cancelled {
                cause: CancelCause::Requested(_) | CancelCause::Deadline,
            })
        )
    {
        // Do not overwrite the original unresolved model/tool identity with a
        // cleanup result. The HostPort owns every accepted capture receipt even
        // after this waiter returns; a capture Unknown is never replayed here.
        let _capture_outcome = capture_turn_advisory(
            host,
            &fresh_capture_cleanup(execution),
            input,
            messages,
            false,
        );
    }
    original
}

fn is_deadline_unknown(error: &PortError, control: &CallControl) -> bool {
    matches!(error, PortError::AcceptedUnknown { .. })
        && control.cancellation.hard_stop_cause().is_none()
        && matches!(
            control.check(),
            Err(PortError::Cancelled {
                cause: CancelCause::Deadline,
            })
        )
}

/// Source's capture hook is advisory for known failures. An accepted effect
/// whose reply is missing retains its owner identity and prevents completion.
fn capture_turn_advisory(
    host: &mut dyn HostPort,
    control: &CallControl,
    input: &TurnInput,
    messages: &[ProjectedMessage],
    errored: bool,
) -> PortResult<()> {
    if !host.advertised("context", "capture_turn") {
        return Ok(());
    }
    match host.call(
        control,
        HostCall::CaptureTurn(crate::ports::CaptureTurn {
            session_id: input.session_id.clone(),
            turn_id: input.turn_id.clone(),
            messages: messages.to_vec(),
            errored,
        }),
    ) {
        Ok(HostReply::CapturedTurn) => Ok(()),
        Ok(_) => Err(wrong_reply("capture_turn")),
        Err(
            error @ (PortError::AcceptedUnknown { .. }
            | PortError::Cancelled { .. }
            | PortError::InvalidResponse { .. }),
        ) => Err(error),
        // A returned Source throw or failure before dispatch has no unresolved
        // accepted capture effect. Preserve the original defensive catch.
        Err(_) => Ok(()),
    }
}
fn structured_output_status() -> BoundarySnapshot {
    let mut detail = JsObject::new();
    for (name, value) in [
        (
            "message",
            "Structured output was returned, so this turn has completed.".into(),
        ),
        (
            "messageI18n",
            i18n("chat:agentStatus.structuredOutputCompleted.message"),
        ),
        ("code", "structured_output_completed".into()),
        ("visible", JsValue::Bool(true)),
        ("scope", "turn".into()),
        ("source", "agent".into()),
    ] {
        detail.insert(name.into(), value);
    }
    BoundarySnapshot::capture(&JsValue::Object(detail))
}
fn max_turns_status(max: f64, label: &str, hint: &str) -> BoundarySnapshot {
    let mut params = JsObject::new();
    params.insert("maxTurns".into(), JsValue::Number(max));
    let mut descriptor = JsObject::new();
    descriptor.insert("key".into(), "chat:agentStatus.maxTurns.message".into());
    descriptor.insert("params".into(), JsValue::Object(params));
    let mut detail = JsObject::new();
    for (name, value) in [
        ("message", JsValue::String(format!("Reached the maximum number of turns ({label}), so this turn has stopped. Increase maxTurns or split the task into smaller steps and try again.").into())),
        ("messageI18n", JsValue::Object(descriptor)),
        ("code", "agent_max_turns_reached".into()),
        ("severity", "error".into()),
        ("visible", JsValue::Bool(true)),
        ("userHint", hint.into()),
        ("userHintI18n", i18n("chat:agentStatus.maxTurns.hint")),
        ("scope", "turn".into()),
        ("source", "agent".into()),
        ("maxTurns", JsValue::Number(max)),
    ] { detail.insert(name.into(), value); }
    BoundarySnapshot::capture(&JsValue::Object(detail))
}
fn known_compaction_advisory(error: &PortError) -> bool {
    match error {
        PortError::AcceptedUnknown { .. }
        | PortError::Cancelled { .. }
        | PortError::InvalidResponse { .. } => false,
        PortError::HostFailure { code, boundary, .. }
        | PortError::ProviderBoundaryOnly { code, boundary, .. } => {
            !code.equals_str("COMPACTION_BUDGET_CONTRACT_INVALID")
                && !boundary
                    .value()
                    .get("code")
                    .and_then(JsValue::as_string)
                    .is_some_and(|code| code.equals_str("COMPACTION_BUDGET_CONTRACT_INVALID"))
        }
        PortError::Provider(error) => !error
            .boundary
            .value()
            .get("code")
            .and_then(JsValue::as_string)
            .is_some_and(|code| code.equals_str("COMPACTION_BUDGET_CONTRACT_INVALID")),
        _ => true,
    }
}
fn profile_gap(profile: &str) -> PortError {
    PortError::InvalidResponse {
        message: format!("Rust normal-turn candidate has not implemented {profile}.").into(),
    }
}
fn empty_usage() -> Usage {
    Usage {
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        total_tokens: None,
        native_cost: None,
    }
}
fn merge_usage(target: &mut Usage, other: Option<&Usage>) {
    let Some(other) = other else {
        return;
    };
    fn add(a: &mut Option<f64>, b: Option<f64>) {
        if a.is_some() || b.is_some() {
            *a = Some(a.unwrap_or(0.0) + b.unwrap_or(0.0));
        }
    }
    add(&mut target.input_tokens, other.input_tokens);
    add(&mut target.output_tokens, other.output_tokens);
    add(&mut target.cache_read_tokens, other.cache_read_tokens);
    add(&mut target.cache_write_tokens, other.cache_write_tokens);
    add(&mut target.total_tokens, other.total_tokens);
    add(&mut target.native_cost, other.native_cost);
}
fn make_result(
    state: &TurnExecutionState,
    started_at: JsString,
    completed_at: JsString,
    kind: TurnResultKind,
    stop_reason: TurnStopReason,
) -> TurnResult {
    TurnResult {
        kind,
        stop_reason,
        final_message: state.final_message.clone(),
        usage: state.usage.clone(),
        permission_denials: state.permission_denials.clone(),
        turns: state.turn_count,
        started_at,
        completed_at,
        structured_output: state.structured_output.clone(),
        errors: Vec::new(),
        budget: state.budget.clone(),
    }
}

fn permission_mode(value: &JsString) -> Option<PermissionMode> {
    if value.equals_str("default") {
        Some(PermissionMode::Default)
    } else if value.equals_str("plan") {
        Some(PermissionMode::Plan)
    } else if value.equals_str("bypassPermissions") {
        Some(PermissionMode::BypassPermissions)
    } else {
        None
    }
}
fn wire_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        JsValue::Object(_) | JsValue::Array(_) => true,
    }
}

fn select_owning_budget_usage(
    assembled: &TurnAssembly,
    estimated_input_tokens: Option<f64>,
    tracks_budget: bool,
) -> PortResult<(Option<BoundarySnapshot>, Option<Usage>)> {
    let original_usage = assembled
        .boundary
        .value()
        .get("usage")
        .filter(|value| !value.is_undefined())
        .map(BoundarySnapshot::capture);
    if !tracks_budget {
        return Ok((original_usage, assembled.usage.clone()));
    }
    use crate::usage_fallback::{
        BudgetUsageInput, BudgetUsageSelection, OriginalBudgetUsageFallback,
    };
    match OriginalBudgetUsageFallback.select(BudgetUsageInput {
        usage: original_usage.as_ref(),
        estimated_input_tokens,
        assistant_message: &assembled.message.boundary,
    })? {
        BudgetUsageSelection::Original(usage) => Ok((usage, assembled.usage.clone())),
        BudgetUsageSelection::NeedsOutputEstimate(continuation) => {
            let count = pilotdeck_tokenizer::count_tokens(continuation.response_text()).map_err(
                |error| PortError::InvalidResponse {
                    message: format!("Ordinary output token counter failed: {error:?}").into(),
                },
            )?;
            let count = u64::try_from(count).map_err(|_| PortError::InvalidResponse {
                message: "Output token count exceeds the accounting integer domain.".into(),
            })?;
            let selected = continuation.resolve(count);
            let view = selected.as_ref().map(project_selected_usage).transpose()?;
            Ok((selected, view))
        }
    }
}
fn project_selected_usage(usage: &BoundarySnapshot) -> PortResult<Usage> {
    fn field(usage: &BoundarySnapshot, key: &str) -> PortResult<Option<f64>> {
        match usage.value().get(key) {
            None | Some(JsValue::Undefined) => Ok(None),
            Some(JsValue::Number(value)) => Ok(Some(*value)),
            _ => Err(profile_gap(
                "selected budget usage numeric aggregate projection",
            )),
        }
    }
    Ok(Usage {
        input_tokens: field(usage, "inputTokens")?,
        output_tokens: field(usage, "outputTokens")?,
        cache_read_tokens: field(usage, "cacheReadTokens")?,
        cache_write_tokens: field(usage, "cacheWriteTokens")?,
        total_tokens: field(usage, "totalTokens")?,
        native_cost: field(usage, "nativeCost")?,
    })
}

fn executed_label(field: &SourceField) -> PortResult<Option<JsString>> {
    match field {
        SourceField::Missing => Ok(None),
        SourceField::Present(value) => match value.value() {
            JsValue::Null | JsValue::Undefined => Ok(None),
            JsValue::String(value) => Ok(Some(value.clone())),
            _ => Err(profile_gap("non-string executed route projection")),
        },
    }
}

fn timeline_error(error: crate::TimelineError, fallback: &BoundarySnapshot) -> PortError {
    PortError::SourceException {
        message: error.message,
        boundary: error.partial_input.unwrap_or_else(|| fallback.clone()),
    }
}
fn annotate_message(timeline: &mut TurnTimeline, message: &mut ProjectedMessage) -> PortResult<()> {
    message.boundary = timeline
        .message(&message.boundary)
        .map_err(|error| timeline_error(error, &message.boundary))?;
    Ok(())
}
fn emit_event(
    events: &mut dyn TurnEventPort,
    timeline: &mut TurnTimeline,
    input: &TurnInput,
    event: TurnEvent,
) -> PortResult<()> {
    let boundary = crate::turn_wire::event(input, &event);
    let boundary = timeline
        .event(&boundary)
        .map_err(|error| timeline_error(error, &boundary))?;
    events.emit(boundary)
}

fn emit_accepted_event(
    events: &mut dyn TurnEventPort,
    timeline: &mut TurnTimeline,
    input: &TurnInput,
    event: TurnEvent,
) -> PortResult<()> {
    let boundary = crate::turn_wire::event(input, &event);
    let boundary = timeline
        .event(&boundary)
        .map_err(|error| timeline_error(error, &boundary))?;
    events.emit_accepted(boundary)
}

fn source_assembly_error(error: SourceAssemblyError, boundary: &BoundarySnapshot) -> PortError {
    let message = match error {
        SourceAssemblyError::TypeError { message }
        | SourceAssemblyError::NativeGraphRequired { message } => message,
    };
    PortError::SourceException {
        message,
        boundary: boundary.clone(),
    }
}
struct AbortedTurnPorts<'a> {
    host: &'a mut dyn HostPort,
    preparation: &'a dyn TurnPreparationPort,
    events: &'a mut dyn TurnEventPort,
    clock: &'a dyn TurnClock,
    input: &'a TurnInput,
    control: &'a CallControl,
}
/// Which Original aborted-stream branch applies (AgentLoop.ts b243fa0).
/// `StreamThrew`: the model iterator threw or was cancelled while the abort
/// signal was set (:859-885) -> partial message, capture, terminal, no status.
/// `StreamCompleted`: the iterator finished and the abort was observed after
/// it (:916-946) -> `turn_aborted` status when the reason matches. Verified by
/// the genuine Original probe original-cancel-branch-probe-v1 (throw=0, eos=1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbortedStreamBranch {
    StreamThrew,
    StreamCompleted,
}
fn finish_aborted_stream(
    mut state: TurnExecutionState,
    started_at: JsString,
    assembled: BoundarySnapshot,
    cause: CancelCause,
    branch: AbortedStreamBranch,
    ports: AbortedTurnPorts<'_>,
) -> PortResult<TurnOutput> {
    let AbortedTurnPorts {
        host,
        preparation,
        events,
        clock,
        input,
        control,
    } = ports;
    let deadline = matches!(cause, CancelCause::Deadline);
    // Source safeFinalTextMessage forbids all pending tool calls, removes only
    // thinking blocks, and keeps the original full message/metadata otherwise.
    let no_tools = matches!(assembled.value().get("toolCalls"), Some(JsValue::Array(calls)) if calls.is_empty());
    if no_tools {
        let admitted = preparation.project_assembly(&assembled)?;
        let mut message = assembled
            .value()
            .get("message")
            .and_then(JsValue::as_object)
            .cloned()
            .ok_or_else(|| wrong_reply("aborted assembled message"))?;
        let content = match message.get("content") {
            Some(JsValue::Array(content)) => content
                .iter()
                .filter(|block| {
                    !block
                        .get("type")
                        .and_then(JsValue::as_string)
                        .is_some_and(|kind| kind.equals_str("thinking"))
                })
                .cloned()
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let has_text = content.iter().any(|block| {
            block
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("text"))
                && block
                    .get("text")
                    .and_then(JsValue::as_string)
                    .is_some_and(|text| text.units().iter().any(|unit| !source_whitespace(*unit)))
        });
        if has_text {
            message.insert("content".into(), JsValue::Array(content));
            let mut message = ProjectedMessage {
                boundary: BoundarySnapshot::capture(&JsValue::Object(message)),
                normalized: MessageProjection::BoundaryOnly(vec![
                    crate::ProjectionGap::OtherBoundaryShape,
                ]),
            };
            annotate_message(&mut state.timeline, &mut message)?;
            state.final_message = Some(message.clone());
            state.messages.push(message.clone());
            expire_consumed_transient_prompts(&mut state);
            merge_usage(&mut state.usage, admitted.usage.as_ref());
            emit_accepted_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::AssistantMessage { message },
            )?;
        }
    }
    if branch == AbortedStreamBranch::StreamCompleted
        && let CancelCause::Requested(reason) = cause
    {
        let text = reason
            .units()
            .iter()
            .map(|unit| {
                if (0x41..=0x5a).contains(unit) {
                    unit + 0x20
                } else {
                    *unit
                }
            })
            .collect::<Vec<_>>();
        if ["timeout", "cancel", "abort"].iter().any(|word| {
            text.windows(word.len())
                .any(|part| part.iter().copied().eq(word.encode_utf16()))
        }) {
            emit_accepted_event(
                events,
                &mut state.timeline,
                input,
                TurnEvent::AgentStatus {
                    event: "turn_aborted".into(),
                    detail: abort_status_detail(reason),
                },
            )?;
        }
    }
    let capture_cleanup = fresh_capture_cleanup(control);
    let capture = capture_turn_advisory(host, &capture_cleanup, input, &state.messages, false);
    // An advisory capture may expire before dispatch when the writer stalls.
    // Execution Deadline still publishes completion; hard stops propagate.
    if !matches!(
        &capture,
        Err(PortError::AcceptedUnknown { .. }
            | PortError::Cancelled {
                cause: CancelCause::Requested(_) | CancelCause::Deadline,
            }) if deadline && capture_cleanup.cancellation.hard_stop_cause().is_none()
    ) {
        capture?;
    }
    let result = make_result(
        &state,
        started_at,
        clock.now_iso(),
        TurnResultKind::Aborted,
        TurnStopReason::AbortedStreaming,
    );
    emit_accepted_event(
        events,
        &mut state.timeline,
        input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(TurnOutput {
        result,
        messages: state.messages,
    })
}
fn abort_status_detail(reason: JsString) -> BoundarySnapshot {
    let mut detail = JsObject::new();
    for (name, value) in [
        (
            "message",
            JsValue::from("This turn was aborted before completion."),
        ),
        ("messageI18n", i18n("chat:agentStatus.turnAborted.message")),
        ("code", JsValue::from("turn_aborted")),
        ("visible", JsValue::Bool(true)),
        (
            "userHint",
            JsValue::from(
                "Retry when you are ready to continue. If this was unexpected, check whether you clicked Stop, switched sessions during a run, or lost the gateway connection.",
            ),
        ),
        ("userHintI18n", i18n("chat:agentStatus.turnAborted.hint")),
        ("scope", JsValue::from("turn")),
        ("source", JsValue::from("agent")),
        ("reason", JsValue::String(reason)),
    ] {
        detail.insert(name.into(), value);
    }
    BoundarySnapshot::capture(&JsValue::Object(detail))
}
fn i18n(key: &str) -> JsValue {
    let mut value = JsObject::new();
    value.insert("key".into(), key.into());
    JsValue::Object(value)
}

fn source_whitespace(unit: u16) -> bool {
    matches!(unit,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}
fn content_filter_status() -> BoundarySnapshot {
    let mut detail = JsObject::new();
    for (name, value) in [
        (
            "message",
            JsValue::from(
                "The response may be incomplete because the model stopped due to content filtering.",
            ),
        ),
        (
            "messageI18n",
            i18n("chat:agentStatus.contentFilter.message"),
        ),
        ("code", JsValue::from("content_filter_stop")),
        ("severity", JsValue::from("warning")),
        ("visible", JsValue::Bool(true)),
        (
            "userHint",
            JsValue::from(
                "Retry with a narrower request or adjust the prompt to avoid filtered content; if this seems wrong, check the provider API policy/status for the selected model.",
            ),
        ),
        ("userHintI18n", i18n("chat:agentStatus.contentFilter.hint")),
        ("scope", JsValue::from("turn")),
        ("source", JsValue::from("agent")),
    ] {
        detail.insert(name.into(), value);
    }
    BoundarySnapshot::capture(&JsValue::Object(detail))
}

fn message_text(message: &JsValue) -> PortResult<JsString> {
    let Some(JsValue::Array(content)) = message.get("content") else {
        return Err(profile_gap("non-array model message text"));
    };
    let mut text = Vec::new();
    let mut first = true;
    for block in content {
        if matches!(block, JsValue::Null | JsValue::Undefined) {
            let receiver = if block.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            return Err(PortError::SourceException {
                message: format!("Cannot read properties of {receiver} (reading 'type')").into(),
                boundary: BoundarySnapshot::capture(block),
            });
        }
        if block
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str("text"))
        {
            if !first {
                text.push(10);
            }
            first = false;
            match block.get("text") {
                Some(JsValue::String(value)) => text.extend_from_slice(value.units()),
                None | Some(JsValue::Null | JsValue::Undefined) => {}
                _ => return Err(profile_gap("non-string model text coercion")),
            }
        }
    }
    Ok(JsString::from_utf16(text))
}
fn stop_block(result: &BoundarySnapshot) -> PortResult<Option<JsValue>> {
    let Some(JsValue::Array(effects)) = result.value().get("effects") else {
        return Err(wrong_reply("Stop lifecycle effects"));
    };
    for effect in effects {
        if matches!(effect, JsValue::Null | JsValue::Undefined) {
            let receiver = if effect.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            return Err(PortError::SourceException {
                message: format!("Cannot read properties of {receiver} (reading 'type')").into(),
                boundary: BoundarySnapshot::capture(effect),
            });
        }
        if effect
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str("block"))
        {
            return Ok(Some(
                effect.get("reason").cloned().unwrap_or(JsValue::Undefined),
            ));
        }
    }
    Ok(None)
}
fn lifecycle_blocked_status(reason: JsValue) -> BoundarySnapshot {
    let text = if wire_truthy(&reason) {
        reason
    } else {
        "A lifecycle hook blocked this turn.".into()
    };
    let mut detail = JsObject::new();
    for (name, value) in [
        ("message", text),
        ("code", "agent_unsupported_feature".into()),
        ("severity", "error".into()),
        ("visible", JsValue::Bool(true)),
        (
            "userHint",
            "Review the blocking lifecycle hook output or disable the hook, then retry.".into(),
        ),
        ("scope", "turn".into()),
        ("source", "agent".into()),
        ("stage", "stop".into()),
    ] {
        detail.insert(name.into(), value);
    }
    BoundarySnapshot::capture(&JsValue::Object(detail))
}

#[cfg(test)]
#[path = "turn_tests/capture_responsibility_tests.rs"]
mod capture_responsibility_tests;

#[cfg(test)]
#[path = "turn_tests/compact_callback_timeline_tests.rs"]
mod compact_callback_timeline_tests;

#[cfg(test)]
#[path = "turn_tests/aborted_stream_branch_tests.rs"]
mod aborted_stream_branch_tests;
#[cfg(test)]
#[path = "turn_tests/owning_cancel_branch_tests.rs"]
mod owning_cancel_branch_tests;
#[cfg(test)]
#[path = "turn_tests/owning_ordinary_recovery_tests.rs"]
mod owning_ordinary_recovery_tests;
#[cfg(test)]
#[path = "turn_tests/pre_model_abort_tests.rs"]
mod pre_model_abort_tests;

fn source_field_equals(field: &SourceField, expected: &JsString) -> bool {
    matches!(field, SourceField::Present(value) if value.value().as_string() == Some(expected))
}
fn source_request_fingerprint(request: &CapturedRequest) -> PortResult<JsString> {
    use sha2::{Digest, Sha256};
    let serialized = pilotdeck_protocol::stringify_json(request.wire.request())
        .map_err(|error| PortError::SourceException {
            message: error.message.into(),
            boundary: BoundarySnapshot::capture(request.wire.request()),
        })?
        .ok_or_else(|| profile_gap("undefined canonical request fingerprint"))?;
    let digest = Sha256::digest(serialized.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(hex.into())
}

enum OutputPlanProgress {
    FallThrough,
    Continue,
    Completed(Box<TurnOutput>),
}
#[allow(clippy::too_many_arguments)]
fn apply_pre_emission_output_plan(
    plan: &crate::output_recovery::OutputRecoveryPlan,
    state: &mut TurnExecutionState,
    started_at: &JsString,
    config: &TurnRuntimeConfig,
    preparation: &mut dyn TurnPreparationPort,
    repair: &crate::LargeFileRepair,
    clock: &dyn TurnClock,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<OutputPlanProgress> {
    use crate::output_recovery::OutputRecoveryAction;
    match &plan.action {
        OutputRecoveryAction::FallThrough => Ok(OutputPlanProgress::FallThrough),
        OutputRecoveryAction::Continue { cap_change, prompt } => {
            if let Some(change) = cap_change {
                preparation
                    .set_output_cap(plan.routed_route(), OutputCapUpdate::Requested(change.next))?;
                emit_event(
                    ports.events,
                    &mut state.timeline,
                    ports.input,
                    TurnEvent::RecoveryCap {
                        route: plan.routed_route().clone(),
                        change: *change,
                        finish_reason: plan
                            .assembled()
                            .value()
                            .get("finishReason")
                            .map(BoundarySnapshot::capture),
                    },
                )?;
            }
            if let Some((prompt, purpose)) = prompt {
                push_transient_synthetic_prompt(state, prompt.clone(), purpose.clone(), clock);
            }
            emit_event(
                ports.events,
                &mut state.timeline,
                ports.input,
                TurnEvent::Continued {
                    reason: TurnContinueReason::ModelError,
                },
            )?;
            Ok(OutputPlanProgress::Continue)
        }
        OutputRecoveryAction::UseLargeFileDecision {
            decision,
            strip_current_assistant,
        } => match decision {
            crate::LargeFileRepairDecision::Continue {
                prompt,
                purpose,
                strip,
            } => {
                if *strip_current_assistant {
                    match strip {
                        Some(crate::RepairStrip::ErrorPair) => strip_trailing_error_pair(state)?,
                        Some(crate::RepairStrip::Assistant) => strip_trailing_assistant(state),
                        None => {}
                    }
                }
                push_transient_synthetic_prompt(state, prompt.clone(), purpose.clone(), clock);
                let current = preparation.current_output_cap(config, &config.route)?;
                if current.is_none_or(|current| current < repair.recommended_max_output_tokens()) {
                    preparation.set_output_cap(
                        &config.route,
                        OutputCapUpdate::Session(repair.recommended_max_output_tokens()),
                    )?;
                }
                emit_event(
                    ports.events,
                    &mut state.timeline,
                    ports.input,
                    TurnEvent::Continued {
                        reason: TurnContinueReason::ModelError,
                    },
                )?;
                Ok(OutputPlanProgress::Continue)
            }
            crate::LargeFileRepairDecision::Stop { reason } => {
                let mut raw = JsObject::new();
                raw.insert("code".into(), "agent_tool_error_loop".into());
                raw.insert("message".into(), JsValue::String(reason.clone()));
                raw.insert("details".into(), JsValue::Undefined);
                let error = BoundarySnapshot::capture(&JsValue::Object(raw));
                let mut result = make_result(
                    state,
                    started_at.clone(),
                    clock.now_iso(),
                    TurnResultKind::Error,
                    TurnStopReason::ToolError,
                );
                result.errors.push(error.clone());
                emit_event(
                    ports.events,
                    &mut state.timeline,
                    ports.input,
                    TurnEvent::AgentStatus {
                        event: "tool_error_loop".into(),
                        detail: large_file_stop_status(reason.clone()),
                    },
                )?;
                finish_output_plan_terminal(state, result, Some(error), ports)
            }
        },
        // Original AgentLoop.ts:1629-1651 residual Phase A: strip the durable
        // error pair, set the requested cap on the decision route, publish
        // token_cap_adjusted (reason max-output-retry-bump) then turn_continued.
        OutputRecoveryAction::StripErrorPairThenContinue { cap_change } => {
            strip_trailing_error_pair(state)?;
            preparation.set_output_cap(
                plan.routed_route(),
                OutputCapUpdate::Requested(cap_change.next),
            )?;
            emit_event(
                ports.events,
                &mut state.timeline,
                ports.input,
                TurnEvent::RecoveryCap {
                    route: plan.routed_route().clone(),
                    change: *cap_change,
                    finish_reason: plan
                        .assembled()
                        .value()
                        .get("finishReason")
                        .map(BoundarySnapshot::capture),
                },
            )?;
            emit_event(
                ports.events,
                &mut state.timeline,
                ports.input,
                TurnEvent::Continued {
                    reason: TurnContinueReason::ModelError,
                },
            )?;
            Ok(OutputPlanProgress::Continue)
        }
        OutputRecoveryAction::VisibleLengthExhausted => Err(profile_gap(
            "output recovery plan at wrong pre-emission phase",
        )),
        OutputRecoveryAction::RepairedExhausted => {
            let error = plan.terminal_error()?;
            let mut result = make_result(
                state,
                started_at.clone(),
                clock.now_iso(),
                TurnResultKind::Error,
                TurnStopReason::ModelError,
            );
            result.errors.push(error.clone());
            emit_output_plan_status(
                plan.terminal_status(Some(&error))?,
                state,
                ports.events,
                ports.input,
            )?;
            finish_output_plan_terminal(state, result, Some(error), ports)
        }
        OutputRecoveryAction::EmptyExhausted { .. } => {
            // Source filters the whole history before selecting .at(-1).
            let mut previous = None;
            for message in &state.messages {
                let value = message.boundary.value();
                if matches!(value, JsValue::Null | JsValue::Undefined) {
                    return Err(PortError::SourceException {
                        message: format!(
                            "Cannot read properties of {} (reading 'role')",
                            if matches!(value, JsValue::Null) {
                                "null"
                            } else {
                                "undefined"
                            }
                        )
                        .into(),
                        boundary: message.boundary.clone(),
                    });
                }
                if value
                    .get("role")
                    .and_then(JsValue::as_string)
                    .is_some_and(|r| r.equals_str("assistant"))
                {
                    previous = Some(message.clone());
                }
            }
            state.final_message = previous;
            emit_output_plan_status(
                plan.terminal_status(None)?,
                state,
                ports.events,
                ports.input,
            )?;
            let mut result = make_result(
                state,
                started_at.clone(),
                clock.now_iso(),
                TurnResultKind::Success,
                TurnStopReason::Completed,
            );
            // This Source terminal deliberately excludes structuredOutput.
            result.structured_output = None;
            capture_turn_advisory(
                ports.host,
                ports.control,
                ports.input,
                &state.messages,
                true,
            )?;
            emit_event(
                ports.events,
                &mut state.timeline,
                ports.input,
                TurnEvent::Completed {
                    result: result.clone(),
                },
            )?;
            Ok(OutputPlanProgress::Completed(Box::new(TurnOutput {
                result,
                messages: state.messages.clone(),
            })))
        }
    }
}
fn large_file_decision_plan(
    decision: crate::LargeFileRepairDecision,
    strip_current_assistant: bool,
    original_request: &BoundarySnapshot,
    routed_route: &Route,
    assembled: &BoundarySnapshot,
) -> crate::output_recovery::OutputRecoveryPlan {
    crate::output_recovery::OutputRecoveryPlan {
        action: crate::output_recovery::OutputRecoveryAction::UseLargeFileDecision {
            decision,
            strip_current_assistant,
        },
        original_request: original_request.clone(),
        routed_route: routed_route.clone(),
        assembled: assembled.clone(),
        output_recovery_count: 0, // unused by the separate six-field repair owner
    }
}
fn strip_trailing_assistant(state: &mut TurnExecutionState) {
    if state
        .messages
        .last()
        .and_then(|m| m.boundary.value().get("role"))
        .and_then(JsValue::as_string)
        .is_some_and(|role| role.equals_str("assistant"))
    {
        state.messages.pop();
    }
}
/// Source removes a trailing all-tool-result user message and then independently
/// removes a trailing assistant. It does not restore finalMessage or structuredOutput.
fn strip_trailing_error_pair(state: &mut TurnExecutionState) -> PortResult<()> {
    if let Some(last) = state.messages.last() {
        let value = last.boundary.value();
        if value
            .get("role")
            .and_then(JsValue::as_string)
            .is_some_and(|role| role.equals_str("user"))
        {
            let undefined = JsValue::Undefined;
            let content = value.get("content").unwrap_or(&undefined);
            let JsValue::Array(blocks) = content else {
                return Err(PortError::SourceException {
                    message: match content {
                        JsValue::Null => "Cannot read properties of null (reading 'every')",
                        JsValue::Undefined => {
                            "Cannot read properties of undefined (reading 'every')"
                        }
                        _ => "last.content.every is not a function",
                    }
                    .into(),
                    boundary: last.boundary.clone(),
                });
            };
            let mut all = true;
            for block in blocks {
                if matches!(block, JsValue::Null | JsValue::Undefined) {
                    return Err(PortError::SourceException {
                        message: format!(
                            "Cannot read properties of {} (reading 'type')",
                            if matches!(block, JsValue::Null) {
                                "null"
                            } else {
                                "undefined"
                            }
                        )
                        .into(),
                        boundary: BoundarySnapshot::capture(block),
                    });
                }
                if !block
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("tool_result"))
                {
                    all = false;
                    break;
                }
            }
            if all {
                state.messages.pop();
            }
        }
    }
    strip_trailing_assistant(state);
    Ok(())
}

// ---------------------------------------------------------------------------
// Original message projections used by reactive model-error recovery. These
// are literal ports of AgentLoop.ts:3166-3187 / 3268-3290 and
// CompactionEngine.ts:505-535, 772-819 + toolPairIntegrity.ts:11-141. They
// read the wire boundary of each message; a JS TypeError on a nullish receiver
// surfaces as PortError::SourceException rather than an invented default.
// ---------------------------------------------------------------------------
const CONTINUATION_TEXT: &str =
    "[system: the conversation above has been compacted. please continue with the current task.]";
const INTERNAL_USER_TEXT_PREFIXES: [&str; 5] = [
    "<compact-boundary",
    "<snip-boundary",
    "<memory-context>",
    "<internal-compaction-control",
    "<hook_context",
];
const IMAGE_REMOVED_TEXT: &str = "[Image removed: multimodal processor error recovery]";

fn js_type_error(text: &str, value: &JsValue) -> PortError {
    PortError::SourceException {
        message: text.into(),
        boundary: BoundarySnapshot::capture(value),
    }
}
fn nullish_name(value: &JsValue) -> &'static str {
    if matches!(value, JsValue::Null) {
        "null"
    } else {
        "undefined"
    }
}
/// `receiver.<property>` where a nullish receiver throws.
fn js_property<'a>(receiver: &'a JsValue, property: &str) -> PortResult<&'a JsValue> {
    if matches!(receiver, JsValue::Null | JsValue::Undefined) {
        return Err(js_type_error(
            &format!(
                "Cannot read properties of {} (reading '{property}')",
                nullish_name(receiver)
            ),
            receiver,
        ));
    }
    Ok(receiver.get(property).unwrap_or(&JS_UNDEFINED))
}
static JS_UNDEFINED: JsValue = JsValue::Undefined;
fn js_string_is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
/// `message.content.<method>(...)` requires an array receiver.
fn content_array<'a>(message: &'a JsValue, method: &str) -> PortResult<&'a [JsValue]> {
    let content = js_property(message, "content")?;
    match content {
        JsValue::Array(blocks) => Ok(blocks),
        JsValue::Null | JsValue::Undefined => Err(js_type_error(
            &format!(
                "Cannot read properties of {} (reading '{method}')",
                nullish_name(content)
            ),
            content,
        )),
        other => Err(js_type_error(
            &format!("message.content.{method} is not a function"),
            other,
        )),
    }
}
/// Source `messageContent` (model/protocol/clone.ts:9-11): non-array is `[]`.
fn message_content(message: &JsValue) -> &[JsValue] {
    match message.get("content") {
        Some(JsValue::Array(blocks)) => blocks,
        _ => &[],
    }
}
/// `{ ...message, content }`: the existing key keeps its position.
fn replace_content(message: &JsValue, content: Vec<JsValue>) -> ProjectedMessage {
    let mut object = message.as_object().cloned().unwrap_or_default();
    object.insert("content".into(), JsValue::Array(content));
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(&JsValue::Object(object)),
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
    }
}
fn js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}
fn js_trim(units: &[u16]) -> &[u16] {
    let start = units
        .iter()
        .position(|u| !js_whitespace(*u))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !js_whitespace(*u))
        .map_or(start, |i| i + 1);
    &units[start..end.max(start)]
}
fn starts_with_str(units: &[u16], prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    units.starts_with(&prefix)
}
/// `block.text.<method>` for a text block: nullish throws, non-string is not callable.
fn block_text_units<'a>(block: &'a JsValue, method: &str) -> PortResult<&'a [u16]> {
    let text = js_property(block, "text")?;
    match text {
        JsValue::String(text) => Ok(text.units()),
        JsValue::Null | JsValue::Undefined => Err(js_type_error(
            &format!(
                "Cannot read properties of {} (reading '{method}')",
                nullish_name(text)
            ),
            text,
        )),
        other => Err(js_type_error(
            &format!("block.text.{method} is not a function"),
            other,
        )),
    }
}

/// Original `hasReplayableReasoningContent` (AgentLoop.ts:3184-3189).
fn has_replayable_reasoning_content(content: &[JsValue]) -> PortResult<bool> {
    for block in content {
        if !js_string_is(js_property(block, "type")?, "thinking") {
            continue;
        }
        let reasoning = js_property(block, "reasoningContent")?;
        let replay = if matches!(reasoning, JsValue::Null | JsValue::Undefined) {
            js_property(block, "text")?
        } else {
            reasoning
        };
        // `(reasoningContent ?? text).length > 0`
        let length_positive = match replay {
            JsValue::String(value) => !value.is_empty(),
            JsValue::Array(items) => !items.is_empty(),
            JsValue::Null | JsValue::Undefined => {
                return Err(js_type_error(
                    &format!(
                        "Cannot read properties of {} (reading 'length')",
                        nullish_name(replay)
                    ),
                    replay,
                ));
            }
            _ => false,
        };
        if length_positive || reasoning.as_string().is_some_and(JsString::is_empty) {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Original `addEmptyReasoningContentMarkers` (AgentLoop.ts:3166-3182).
fn add_empty_reasoning_content_markers(
    messages: &[ProjectedMessage],
) -> PortResult<Vec<ProjectedMessage>> {
    let last = messages.len().saturating_sub(1);
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let value = message.boundary.value();
            let assistant = js_string_is(js_property(value, "role")?, "assistant");
            let content = message_content(value);
            if index == last && assistant && content.is_empty() {
                return Ok(message.clone());
            }
            if !assistant || has_replayable_reasoning_content(content)? {
                return Ok(message.clone());
            }
            let mut marker = JsObject::new();
            marker.insert("type".into(), "thinking".into());
            marker.insert("text".into(), "".into());
            marker.insert("reasoningContent".into(), "".into());
            let mut blocks = Vec::with_capacity(content.len() + 1);
            blocks.push(JsValue::Object(marker));
            blocks.extend(content.iter().cloned());
            Ok(replace_content(value, blocks))
        })
        .collect()
}
/// Original `stripImagesFromMessages` (AgentLoop.ts:3268-3290). Source rebuilds
/// every object; an unchanged message is kept as the identical projection.
fn strip_images_from_messages(messages: &[ProjectedMessage]) -> PortResult<Vec<ProjectedMessage>> {
    let placeholder = || {
        let mut text = JsObject::new();
        text.insert("type".into(), "text".into());
        text.insert("text".into(), IMAGE_REMOVED_TEXT.into());
        JsValue::Object(text)
    };
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        let value = message.boundary.value();
        let mut changed = false;
        let mut blocks = Vec::new();
        for block in content_array(value, "map")? {
            let kind = js_property(block, "type")?;
            if js_string_is(kind, "image") {
                changed = true;
                blocks.push(placeholder());
                continue;
            }
            if js_string_is(kind, "tool_result") {
                let inner = content_array(block, "some")?;
                let mut has_image = false;
                for item in inner {
                    if js_string_is(js_property(item, "type")?, "image") {
                        has_image = true;
                    }
                }
                if has_image {
                    let mut mapped = Vec::with_capacity(inner.len());
                    for item in inner {
                        if js_string_is(js_property(item, "type")?, "image") {
                            mapped.push(placeholder());
                        } else {
                            mapped.push(item.clone());
                        }
                    }
                    let mut object = block.as_object().cloned().unwrap_or_default();
                    object.insert("content".into(), JsValue::Array(mapped));
                    changed = true;
                    blocks.push(JsValue::Object(object));
                    continue;
                }
            }
            blocks.push(block.clone());
        }
        out.push(if changed {
            replace_content(value, blocks)
        } else {
            message.clone()
        });
    }
    Ok(out)
}

fn is_direct_tool_result_block(kind: &JsValue) -> bool {
    js_string_is(kind, "tool_result") || js_string_is(kind, "tool_result_reference")
}
fn is_media_reference_with_id(block: &JsValue, kind: &JsValue) -> bool {
    js_string_is(kind, "media_reference")
        && block
            .get("toolCallId")
            .and_then(JsValue::as_string)
            .is_some_and(|id| !id.is_empty())
}
/// toolPairIntegrity.ts:11-20.
fn collect_tool_call_ids(messages: &[ProjectedMessage]) -> PortResult<Vec<JsValue>> {
    let mut ids = Vec::new();
    for message in messages {
        let value = message.boundary.value();
        if !js_string_is(js_property(value, "role")?, "assistant") {
            continue;
        }
        for block in content_array(value, "iterator")? {
            if js_string_is(js_property(block, "type")?, "tool_call") {
                ids.push(js_property(block, "id")?.clone());
            }
        }
    }
    Ok(ids)
}
/// toolPairIntegrity.ts:22-33.
fn collect_tool_result_ids(messages: &[ProjectedMessage]) -> PortResult<Vec<JsValue>> {
    let mut ids = Vec::new();
    for message in messages {
        let value = message.boundary.value();
        if !js_string_is(js_property(value, "role")?, "user") {
            continue;
        }
        for block in content_array(value, "iterator")? {
            if is_direct_tool_result_block(js_property(block, "type")?) {
                ids.push(js_property(block, "toolCallId")?.clone());
            }
        }
    }
    Ok(ids)
}
/// `.filter((m) => m.content.length > 0)` applied to every mapped message.
fn content_length_positive(message: &JsValue) -> PortResult<bool> {
    Ok(match js_property(message, "content")? {
        JsValue::Array(items) => !items.is_empty(),
        JsValue::String(text) => !text.is_empty(),
        content @ (JsValue::Null | JsValue::Undefined) => {
            return Err(js_type_error(
                &format!(
                    "Cannot read properties of {} (reading 'length')",
                    nullish_name(content)
                ),
                content,
            ));
        }
        _ => false,
    })
}
/// toolPairIntegrity.ts:39-53 and 59-74 share one shape: filter the blocks of
/// one role, keep the identical message when nothing was removed, then drop
/// every message whose content became empty.
fn strip_unpaired_blocks(
    messages: &[ProjectedMessage],
    role: &str,
    mut keep: impl FnMut(&JsValue) -> PortResult<bool>,
) -> PortResult<Vec<ProjectedMessage>> {
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        let value = message.boundary.value();
        let mapped = if js_string_is(js_property(value, "role")?, role) {
            let content = content_array(value, "filter")?;
            let mut filtered = Vec::with_capacity(content.len());
            for block in content {
                if keep(block)? {
                    filtered.push(block.clone());
                }
            }
            if filtered.len() == content.len() {
                message.clone()
            } else {
                replace_content(value, filtered)
            }
        } else {
            message.clone()
        };
        if content_length_positive(mapped.boundary.value())? {
            out.push(mapped);
        }
    }
    Ok(out)
}
fn strip_unpaired_tool_calls(
    messages: &[ProjectedMessage],
    paired: &[JsValue],
) -> PortResult<Vec<ProjectedMessage>> {
    strip_unpaired_blocks(messages, "assistant", |block| {
        Ok(!js_string_is(js_property(block, "type")?, "tool_call")
            || paired.contains(js_property(block, "id")?))
    })
}
fn strip_unpaired_tool_results(
    messages: &[ProjectedMessage],
    paired: &[JsValue],
) -> PortResult<Vec<ProjectedMessage>> {
    strip_unpaired_blocks(messages, "user", |block| {
        let kind = js_property(block, "type")?;
        Ok(
            (!is_direct_tool_result_block(kind) && !is_media_reference_with_id(block, kind))
                || paired.contains(js_property(block, "toolCallId")?),
        )
    })
}
/// toolPairIntegrity.ts:101-122.
fn is_real_user_request_message(message: &ProjectedMessage) -> PortResult<bool> {
    let value = message.boundary.value();
    if !js_string_is(js_property(value, "role")?, "user")
        || value
            .get("metadata")
            .and_then(|metadata| metadata.get("synthetic"))
            == Some(&JsValue::Bool(true))
    {
        return Ok(false);
    }
    for block in content_array(value, "some")? {
        let kind = js_property(block, "type")?;
        if is_direct_tool_result_block(kind) || is_media_reference_with_id(block, kind) {
            continue;
        }
        if !js_string_is(kind, "text") {
            return Ok(true);
        }
        let text = js_trim(block_text_units(block, "trim")?);
        if !text.is_empty()
            && !text.iter().copied().eq(CONTINUATION_TEXT.encode_utf16())
            && !INTERNAL_USER_TEXT_PREFIXES
                .iter()
                .any(|prefix| starts_with_str(text, prefix))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
/// toolPairIntegrity.ts:125-137.
fn ensure_trailing_user_message(
    mut messages: Vec<ProjectedMessage>,
) -> PortResult<Vec<ProjectedMessage>> {
    let Some(last) = messages.last() else {
        return Ok(messages);
    };
    if !js_string_is(js_property(last.boundary.value(), "role")?, "assistant") {
        return Ok(messages);
    }
    let mut text = JsObject::new();
    text.insert("type".into(), "text".into());
    text.insert("text".into(), CONTINUATION_TEXT.into());
    let mut message = JsObject::new();
    message.insert("role".into(), "user".into());
    message.insert(
        "content".into(),
        JsValue::Array(vec![JsValue::Object(text)]),
    );
    messages.push(ProjectedMessage {
        boundary: BoundarySnapshot::capture(&JsValue::Object(message)),
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
    });
    Ok(messages)
}
/// CompactionEngine.ts:772-777 (`clamp` at 817-820: NaN -> min).
fn truncate_head(messages: &[ProjectedMessage], keep_ratio: f64) -> &[ProjectedMessage] {
    let ratio = if keep_ratio.is_nan() {
        0.05
    } else {
        keep_ratio.clamp(0.05, 1.0)
    };
    let keep = (messages.len() as f64 * ratio).floor().max(1.0) as usize;
    &messages[messages.len().saturating_sub(keep)..]
}
/// CompactionEngine.ts:794-815.
fn truncate_tail_preserving_tool_pairs(
    messages: &[ProjectedMessage],
    keep_ratio: f64,
) -> PortResult<Vec<ProjectedMessage>> {
    if messages.is_empty() {
        return Ok(Vec::new());
    }
    let raw_tail = truncate_head(messages, keep_ratio);
    let tail_start = messages.len() - raw_tail.len();
    let mut anchor = None;
    for index in (0..=tail_start).rev() {
        if is_real_user_request_message(&messages[index])? {
            anchor = Some(index);
            break;
        }
    }
    let live_tail: Vec<ProjectedMessage> = match anchor {
        Some(anchor) if anchor < tail_start => std::iter::once(messages[anchor].clone())
            .chain(raw_tail.iter().cloned())
            .collect(),
        _ => raw_tail.to_vec(),
    };
    let result_ids = collect_tool_result_ids(&live_tail)?;
    let paired_calls = strip_unpaired_tool_calls(&live_tail, &result_ids)?;
    let call_ids = collect_tool_call_ids(&paired_calls)?;
    strip_unpaired_tool_results(&paired_calls, &call_ids)
}
fn is_compact_boundary_message(message: &ProjectedMessage) -> PortResult<bool> {
    let value = message.boundary.value();
    if !js_string_is(js_property(value, "role")?, "user") {
        return Ok(false);
    }
    for block in content_array(value, "some")? {
        if js_string_is(js_property(block, "type")?, "text")
            && starts_with_str(block_text_units(block, "startsWith")?, "<compact-boundary")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn is_wrapped_summary_message(message: &ProjectedMessage) -> PortResult<bool> {
    let value = message.boundary.value();
    if !js_string_is(js_property(value, "role")?, "assistant") {
        return Ok(false);
    }
    for block in content_array(value, "some")? {
        if js_string_is(js_property(block, "type")?, "text")
            && starts_with_str(
                block_text_units(block, "startsWith")?,
                COMPACT_SUMMARY_PREFIX,
            )
        {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Original `truncateHeadKeepRatio` (AgentLoop.ts:3226) ->
/// `truncateHeadPreservingCheckpoint` (CompactionEngine.ts:780-792) with
/// `splitCheckpointPrefix` (505-526).
fn truncate_head_preserving_checkpoint(
    messages: &[ProjectedMessage],
    keep_ratio: f64,
) -> PortResult<Vec<ProjectedMessage>> {
    let mut index = 0;
    while index + 1 < messages.len()
        && is_compact_boundary_message(&messages[index])?
        && is_wrapped_summary_message(&messages[index + 1])?
    {
        index += 2;
    }
    let (stable_prefix, live) = messages.split_at(index);
    let mut out = stable_prefix.to_vec();
    out.extend(truncate_tail_preserving_tool_pairs(live, keep_ratio)?);
    ensure_trailing_user_message(out)
}

fn emit_output_plan_status(
    status: BoundarySnapshot,
    state: &mut TurnExecutionState,
    events: &mut dyn TurnEventPort,
    input: &TurnInput,
) -> PortResult<()> {
    let event = status
        .value()
        .get("event")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| wrong_reply("output status event"))?;
    let detail = status
        .value()
        .get("detail")
        .map(BoundarySnapshot::capture)
        .ok_or_else(|| wrong_reply("output status detail"))?;
    emit_event(
        events,
        &mut state.timeline,
        input,
        TurnEvent::AgentStatus { event, detail },
    )
}
fn finish_output_plan_terminal(
    state: &mut TurnExecutionState,
    result: TurnResult,
    error: Option<BoundarySnapshot>,
    ports: TerminalTurnPorts<'_>,
) -> PortResult<OutputPlanProgress> {
    if let Some(error) = error {
        emit_event(
            ports.events,
            &mut state.timeline,
            ports.input,
            TurnEvent::Failed { error },
        )?;
    }
    capture_turn_advisory(
        ports.host,
        ports.control,
        ports.input,
        &state.messages,
        true,
    )?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::Completed {
            result: result.clone(),
        },
    )?;
    Ok(OutputPlanProgress::Completed(Box::new(TurnOutput {
        result,
        messages: state.messages.clone(),
    })))
}
fn large_file_stop_status(reason: JsString) -> BoundarySnapshot {
    let text = if reason.is_empty() {
        "The agent repeatedly hit the same tool error, so this turn has stopped.".into()
    } else {
        reason
    };
    let mut raw = JsObject::new();
    for (key, value) in [
        ("message", JsValue::String(text)),
        ("code", "agent_tool_error_loop".into()), ("severity", "error".into()), ("visible", JsValue::Bool(true)),
        ("userHint", "Change the request to avoid repeating the same failing tool call, grant any required permission, or switch to a model with stronger tool-calling support in Settings → Model Provider.".into()),
        ("scope", "turn".into()), ("source", "agent".into()),
    ] { raw.insert(key.into(), value); }
    BoundarySnapshot::capture(&JsValue::Object(raw))
}

#[cfg(test)]
#[path = "turn_tests/ordinary_model_error_source_tests.rs"]
mod ordinary_model_error_source_tests;

#[cfg(test)]
#[path = "turn_tests/ordinary_model_error_terminal_order_tests.rs"]
mod ordinary_model_error_terminal_order_tests;

#[cfg(test)]
#[path = "turn_tests/ordinary_model_error_recovery_tests.rs"]
mod ordinary_model_error_recovery_tests;

#[cfg(test)]
#[path = "turn_tests/owning_reactive_recovery_tests.rs"]
mod owning_reactive_recovery_tests;
