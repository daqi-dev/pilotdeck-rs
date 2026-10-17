//! Owning AgentLoop turn policy. IO/identity/serialization remain bound ports.
use crate::ports::{PermissionMode, Route, RuntimeContextSurface};
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
    AgentStatusText {
        event: JsString,
        text: JsString,
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
            }) if deadline
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
            }) if deadline
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
    let text = status
        .value()
        .get("text")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| wrong_reply("model failure status text"))?;
    let detail = status
        .value()
        .get("detail")
        .map(BoundarySnapshot::capture)
        .ok_or_else(|| wrong_reply("model failure status detail"))?;
    emit_event(
        ports.events,
        &mut state.timeline,
        ports.input,
        TurnEvent::AgentStatusText {
            event,
            text,
            detail,
        },
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
            }) if deadline
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
mod capture_responsibility_tests {
    use super::*;
    use crate::ports::{Cancellation, ExecutionIdentity};

    struct CaptureHost {
        reply: Option<PortResult<HostReply>>,
        calls: usize,
        advertised: bool,
    }
    impl HostPort for CaptureHost {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            self.advertised && module == "context" && operation == "capture_turn"
        }
        fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
            self.calls += 1;
            let HostCall::CaptureTurn(capture) = call else {
                panic!("unexpected effect")
            };
            assert!(capture.errored);
            assert!(capture.messages.is_empty());
            assert!(capture.session_id.equals_str("capture-session"));
            self.reply.take().expect("capture must never be replayed")
        }
    }
    fn input() -> TurnInput {
        TurnInput {
            session_id: "capture-session".into(),
            turn_id: "capture-turn".into(),
            messages: vec![],
            max_turns: None,
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        }
    }
    fn run(reply: PortResult<HostReply>) -> (PortResult<()>, usize) {
        let mut host = CaptureHost {
            reply: Some(reply),
            calls: 0,
            advertised: true,
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let result = capture_turn_advisory(&mut host, &control, &input(), &[], true);
        (result, host.calls)
    }
    #[test]
    fn capture_unknown_retains_full_owner_and_sequence_without_replay() {
        let identity = ExecutionIdentity {
            session_id: "capture-session".into(),
            turn_id: "capture-turn".into(),
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "request".into(),
            idempotency_key: Some("stable-key".into()),
            module_instance_id: "module".into(),
            connection_generation: "generation".into(),
        };
        let (result, calls) = run(Err(PortError::AcceptedUnknown {
            identity: Box::new(identity.clone()),
            last_applied_sequence: Some(19.0),
        }));
        let Err(PortError::AcceptedUnknown {
            identity: retained,
            last_applied_sequence,
        }) = result
        else {
            panic!("unknown effect must not become completion")
        };
        assert_eq!(*retained, identity);
        assert_eq!(last_applied_sequence, Some(19.0));
        assert_eq!(calls, 1);
    }
    #[test]
    fn capture_known_source_failure_remains_advisory() {
        let (result, calls) = run(Err(PortError::HostFailure {
            code: "known_capture_failure".into(),
            message: "returned failure".into(),
            boundary: BoundarySnapshot::capture(&JsValue::Null),
        }));
        assert!(result.is_ok());
        assert_eq!(calls, 1);
    }
    #[test]
    fn capture_cancelled_is_not_reclassified_as_known_failure() {
        let (result, calls) = run(Err(PortError::Cancelled {
            cause: CancelCause::TransportClosed,
        }));
        assert!(matches!(
            result,
            Err(PortError::Cancelled {
                cause: CancelCause::TransportClosed
            })
        ));
        assert_eq!(calls, 1);
    }
    #[test]
    fn capture_reply_for_another_operation_is_invalid() {
        let (result, calls) = run(Ok(HostReply::BudgetCost(Some(1.0))));
        assert!(matches!(result, Err(PortError::InvalidResponse { .. })));
        assert_eq!(calls, 1);
    }
    #[test]
    fn absent_capture_hook_never_dispatches_an_effect() {
        let mut host = CaptureHost {
            reply: None,
            calls: 0,
            advertised: false,
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        assert!(capture_turn_advisory(&mut host, &control, &input(), &[], true).is_ok());
        assert_eq!(host.calls, 0);
    }
}

#[cfg(test)]
mod compact_callback_timeline_tests {
    use super::*;
    fn boundary(input: &str) -> crate::ports::CompactBoundary {
        crate::ports::CompactBoundary {
            boundary: BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(input).unwrap()),
        }
    }
    #[test]
    fn callback_compaction_reuses_position_and_overwrites_stale_timeline() {
        let mut timeline = TurnTimeline::new("turn".into());
        let first = annotate_compaction_boundary(&mut timeline,
            boundary(r#"{"kind":"compact","compactMetadata":{"compactionId":"same","timeline":{"stale":true}}}"#)).unwrap();
        timeline.position("message:later".into(), false);
        let again = annotate_compaction_boundary(
            &mut timeline,
            boundary(r#"{"kind":"compact","compactMetadata":{"compactionId":"same"}}"#),
        )
        .unwrap();
        let position = |b: &crate::ports::CompactBoundary| {
            b.boundary
                .value()
                .get("compactMetadata")
                .unwrap()
                .get("timeline")
                .unwrap()
                .clone()
        };
        assert_eq!(position(&first), position(&again));
        assert_eq!(
            position(&first).get("id"),
            Some(&JsValue::from("compact:same"))
        );
        assert_eq!(position(&first).get("order"), Some(&JsValue::Number(0.0)));
        assert!(position(&first).get("stale").is_none());
    }
    #[test]
    fn falsy_compaction_id_does_not_allocate_or_overwrite_callback_timeline() {
        let mut timeline = TurnTimeline::new("turn".into());
        let original = boundary(
            r#"{"kind":"compact","compactMetadata":{"compactionId":0,"timeline":{"stale":true}}}"#,
        );
        let result = annotate_compaction_boundary(&mut timeline, original.clone()).unwrap();
        assert_eq!(result.boundary.value(), original.boundary.value());
        assert_eq!(timeline.position("next".into(), false).order, 0.0);
    }
}

#[cfg(test)]
mod aborted_stream_branch_tests {
    //! Source-derived regression for the two Original aborted-stream branches
    //! (AgentLoop.ts:859-885 vs :916-946), confirmed by the genuine Original
    //! probe original-cancel-branch-probe-v1: during_stream_throw -> 0 status,
    //! during_stream_eos -> 1 status, quiet reason -> 0 status.
    use super::*;
    use crate::ports::Cancellation;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<&'static str>>>;
    struct Clock(Log);
    impl TurnClock for Clock {
        fn now_iso(&self) -> JsString {
            self.0.lock().unwrap().push("result_clock");
            "2026-01-01T00:00:00.000Z".into()
        }
        fn uuid(&self) -> JsString {
            "id".into()
        }
    }
    struct Preparation;
    impl TurnPreparationPort for Preparation {
        fn finish_pending_effects(&mut self) -> PortResult<()> {
            panic!("aborted stream path has no pending fence")
        }
        fn prepare(
            &mut self,
            _: &mut dyn HostPort,
            _: &CallControl,
            _: &TurnRuntimeConfig,
            _: &TurnInput,
            _: &[ProjectedMessage],
        ) -> PortResult<(CapturedRequest, ModelHints)> {
            panic!("no preparation after abort")
        }
        fn project_assembly(&self, _: &BoundarySnapshot) -> PortResult<TurnAssembly> {
            panic!("pending tool calls forbid a partial message")
        }
    }
    struct Host(Log);
    impl HostPort for Host {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            module == "context" && operation == "capture_turn"
        }
        fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
            assert!(control.deadline.is_some());
            let HostCall::CaptureTurn(capture) = call else {
                panic!("no new model/tool effect")
            };
            assert!(!capture.errored);
            self.0.lock().unwrap().push("capture_false");
            Ok(HostReply::CapturedTurn)
        }
    }
    struct Events(Log);
    impl TurnEventPort for Events {
        fn emit(&mut self, _: BoundarySnapshot) -> PortResult<()> {
            panic!("cancelled execution must use accepted delivery")
        }
        fn emit_accepted(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            let kind = event
                .value()
                .get("type")
                .and_then(JsValue::as_string)
                .unwrap();
            let label = if kind.equals_str("agent_status") {
                "status"
            } else if kind.equals_str("turn_completed") {
                "completed"
            } else {
                panic!("unexpected terminal event")
            };
            self.0.lock().unwrap().push(label);
            Ok(())
        }
    }
    fn message(text: &str) -> ProjectedMessage {
        let mut value = JsObject::new();
        value.insert("role".into(), "user".into());
        value.insert("id".into(), text.into());
        ProjectedMessage {
            boundary: BoundarySnapshot::capture(&JsValue::Object(value)),
            normalized: MessageProjection::BoundaryOnly(vec![
                crate::ProjectionGap::OtherBoundaryShape,
            ]),
        }
    }
    fn input() -> TurnInput {
        TurnInput {
            session_id: "session".into(),
            turn_id: "turn".into(),
            messages: vec![],
            max_turns: None,
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        }
    }
    /// A pending tool call makes Source `safeFinalTextMessage` return nothing,
    /// so only the status/capture/terminal sequence is observed.
    fn assembled_with_pending_tool_call() -> BoundarySnapshot {
        let mut call = JsObject::new();
        call.insert("id".into(), "call-1".into());
        call.insert("name".into(), "lookup".into());
        let mut message = JsObject::new();
        message.insert("role".into(), "assistant".into());
        message.insert("content".into(), JsValue::Array(vec![]));
        let mut value = JsObject::new();
        value.insert(
            "toolCalls".into(),
            JsValue::Array(vec![JsValue::Object(call)]),
        );
        value.insert("message".into(), JsValue::Object(message));
        BoundarySnapshot::capture(&JsValue::Object(value))
    }
    fn run(branch: AbortedStreamBranch, reason: &str) -> Vec<&'static str> {
        let log = Arc::new(Mutex::new(vec![]));
        let state = TurnExecutionState {
            messages: vec![message("prior"), message("prompt")],
            turn_count: 1.0,
            usage: empty_usage(),
            permission_denials: vec![],
            final_message: None,
            structured_output: None,
            budget: BudgetLedger::new(None, None, 0.0),
            timeline: TurnTimeline::new("turn".into()),
            transient_prompt_ids: vec![],
        };
        let mut host = Host(log.clone());
        let preparation = Preparation;
        let mut events = Events(log.clone());
        let clock = Clock(log.clone());
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let turn_input = input();
        let result = finish_aborted_stream(
            state,
            "started".into(),
            assembled_with_pending_tool_call(),
            CancelCause::Requested(reason.into()),
            branch,
            AbortedTurnPorts {
                host: &mut host,
                preparation: &preparation,
                events: &mut events,
                clock: &clock,
                input: &turn_input,
                control: &control,
            },
        );
        assert!(
            result.is_ok(),
            "aborted stream must complete as known cancellation"
        );
        let observed = log.lock().unwrap();
        observed.clone()
    }
    #[test]
    fn stream_threw_under_parity_cancel_omits_status() {
        assert_eq!(
            run(AbortedStreamBranch::StreamThrew, "parity_cancel"),
            vec!["capture_false", "result_clock", "completed"]
        );
    }
    #[test]
    fn stream_completed_then_parity_cancel_emits_status() {
        assert_eq!(
            run(AbortedStreamBranch::StreamCompleted, "parity_cancel"),
            vec!["status", "capture_false", "result_clock", "completed"]
        );
    }
    #[test]
    fn stream_completed_quiet_reason_omits_status() {
        assert_eq!(
            run(AbortedStreamBranch::StreamCompleted, "user_stop"),
            vec!["capture_false", "result_clock", "completed"]
        );
    }
    #[test]
    fn stream_threw_timeout_reason_omits_status() {
        assert_eq!(
            run(AbortedStreamBranch::StreamThrew, "timeout:run-1"),
            vec!["capture_false", "result_clock", "completed"]
        );
    }
}
#[cfg(test)]
mod owning_cancel_branch_tests {
    //! OWNING-path regressions: each test drives `run_admitted_turn` through
    //! real ports so the branch is selected from the live InvocationDisposition,
    //! not handed to the helper. Each expectation names the genuine Original
    //! probe branch it mirrors (original-cancel-branch-probe-v1.json, Original
    //! b243fa0: loop_top=1 post_create=1 stream_throw=0 stream_eos=1 quiet=0).
    //! Ground truth: AgentLoop.ts:859-885 (iterator threw while aborted -> no
    //! status) vs :916-946 (iterator completed, abort observed -> status when
    //! shouldSurfaceAbortStatus(reason), :394-397).
    use super::*;
    use crate::ports::{
        Cancellation, ExecutionHints, RequestProjection, Route, RuntimeContextSurface, StreamChunk,
        StreamEvent,
    };
    use pilotdeck_protocol::model::PreparedWireRequest;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;
    fn push(log: &Log, entry: impl Into<String>) {
        log.lock().unwrap().push(entry.into());
    }
    fn arm(cancellation: &Cancellation, reason: &str) {
        assert!(cancellation.cancel(CancelCause::Requested(reason.into())));
    }
    fn json(text: &str) -> JsValue {
        pilotdeck_protocol::parse_json(text).unwrap()
    }
    fn raw_event(text: &str) -> StreamEvent {
        StreamEvent::BoundaryOnly(BoundarySnapshot::capture(&json(text)))
    }
    fn text_batch() -> Vec<StreamEvent> {
        vec![
            raw_event(r#"{"type":"text_delta","text":"partial "}"#),
            raw_event(r#"{"type":"text_delta","text":"answer"}"#),
        ]
    }
    /// How the fake model host behaves on the second `stream_next` pull. The
    /// first pull always returns a text batch with `done:false`, so both stream
    /// branches own the same partial assistant text before the abort fires.
    #[derive(Clone, Copy)]
    enum StreamPlan {
        /// Arm the abort, then the iterator throws (:859 catch while aborted).
        ThrowWhileAborted,
        /// Arm the abort, then the iterator completes normally (`done:true`),
        /// so the abort is observed only after the stream (:916).
        CompleteThenAbort,
        /// A pre-stream abort must never reach the model host at all.
        Unreached,
        DeadlineUnknown,
        DeadlineUnknownBadAssembly,
    }
    struct Host {
        log: Log,
        plan: StreamPlan,
        cancellation: Cancellation,
        reason: &'static str,
        pulls: usize,
    }
    impl HostPort for Host {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            matches!(
                (module, operation),
                ("context", "capture_turn") | ("model", "stream_next")
            )
        }
        fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
            match call {
                HostCall::ModelPull { .. } => {
                    assert!(
                        control.check().is_ok(),
                        "the owning loop never pulls with the abort already set"
                    );
                    self.pulls += 1;
                    push(&self.log, format!("host:stream_next:{}", self.pulls));
                    if self.pulls == 1 {
                        return Ok(HostReply::ModelChunk(StreamChunk {
                            events: if matches!(self.plan, StreamPlan::DeadlineUnknownBadAssembly) {
                                vec![raw_event(
                                    r#"{"type":"tool_call_end","toolCall":{"id":true,"name":"owned_tool","input":{}}}"#,
                                )]
                            } else {
                                text_batch()
                            },
                            done: false,
                        }));
                    }
                    match self.plan {
                        StreamPlan::ThrowWhileAborted => {
                            arm(&self.cancellation, self.reason);
                            Err(PortError::HostFailure {
                                code: "model_stream_failed".into(),
                                message: "The operation was aborted".into(),
                                boundary: BoundarySnapshot::capture(&JsValue::Null),
                            })
                        }
                        StreamPlan::CompleteThenAbort => {
                            arm(&self.cancellation, self.reason);
                            Ok(HostReply::ModelChunk(StreamChunk {
                                events: vec![raw_event(r#"{"type":"message_end"}"#)],
                                done: true,
                            }))
                        }
                        StreamPlan::DeadlineUnknown | StreamPlan::DeadlineUnknownBadAssembly => {
                            assert!(self.cancellation.cancel(CancelCause::Deadline));
                            Err(PortError::AcceptedUnknown {
                                identity: Box::new(crate::ports::ExecutionIdentity {
                                    session_id: "session".into(),
                                    turn_id: "turn".into(),
                                    run_id: "run".into(),
                                    operation_id: "op".into(),
                                    request_id: "accepted-model".into(),
                                    idempotency_key: None,
                                    module_instance_id: "module".into(),
                                    connection_generation: "generation".into(),
                                }),
                                last_applied_sequence: Some(7.0),
                            })
                        }
                        StreamPlan::Unreached => panic!("pre-stream abort reached the model host"),
                    }
                }
                HostCall::CaptureTurn(capture) => {
                    assert!(control.deadline.is_some(), "capture uses bounded cleanup");
                    assert!(!capture.errored);
                    push(
                        &self.log,
                        format!("host:capture_turn:{}", capture.messages.len()),
                    );
                    Ok(HostReply::CapturedTurn)
                }
                other => panic!("unexpected host call {:?}", other.method()),
            }
        }
    }
    struct Preparation {
        log: Log,
        arm_in_prepare: Option<(Cancellation, &'static str)>,
    }
    impl TurnPreparationPort for Preparation {
        fn prepare(
            &mut self,
            _: &mut dyn HostPort,
            _: &CallControl,
            _: &TurnRuntimeConfig,
            _: &TurnInput,
            _: &[ProjectedMessage],
        ) -> PortResult<(CapturedRequest, ModelHints)> {
            push(&self.log, "prepare");
            if let Some((cancellation, reason)) = self.arm_in_prepare.take() {
                // Original post_create probe: the abort fires inside
                // prepare_for_model and is observed after createModelRequest.
                arm(&cancellation, reason);
            }
            Ok((
                CapturedRequest {
                    route: route(),
                    normalized: RequestProjection::BoundaryOnly(Vec::new()),
                    wire: PreparedWireRequest::capture(&json(
                        r#"{"provider":"test","model":"model","messages":[]}"#,
                    )),
                },
                ModelHints {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    execution: execution(),
                    workspace_id: None,
                    storage_config_version: None,
                    metadata: None,
                    model_override: None,
                },
            ))
        }
        fn project_assembly(&self, boundary: &BoundarySnapshot) -> PortResult<TurnAssembly> {
            push(&self.log, "project_assembly");
            let message = boundary
                .value()
                .get("message")
                .cloned()
                .expect("assembled message");
            Ok(TurnAssembly {
                boundary: boundary.clone(),
                message: ProjectedMessage {
                    boundary: BoundarySnapshot::capture(&message),
                    normalized: MessageProjection::BoundaryOnly(vec![
                        crate::ProjectionGap::OtherBoundaryShape,
                    ]),
                },
                usage: None,
                has_repaired_tool_calls: None,
                has_message_end: true,
            })
        }
    }
    struct Events(Log);
    impl Events {
        fn label(event: &BoundarySnapshot) -> String {
            let value = event.value();
            let kind = value
                .get("type")
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap();
            if kind == "agent_status" {
                let status = value
                    .get("event")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap();
                format!("agent_status:{status}")
            } else {
                kind
            }
        }
    }
    impl TurnEventPort for Events {
        fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            let label = Self::label(&event);
            assert_eq!(
                label, "model_request_started",
                "after the abort every event must use accepted delivery"
            );
            push(&self.0, label);
            Ok(())
        }
        fn emit_accepted(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            let label = Self::label(&event);
            if label == "assistant_message" {
                assert_partial_text(event.value().get("message").expect("assistant message"));
            }
            if label == "turn_completed" {
                let result = event.value().get("result").expect("completion result");
                assert_eq!(result.get("type"), Some(&JsValue::from("aborted")));
                assert_eq!(
                    result.get("stopReason"),
                    Some(&JsValue::from("aborted_streaming"))
                );
                if self
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|entry| entry == "assistant_message")
                {
                    assert_partial_text(
                        result
                            .get("finalMessage")
                            .expect("completed partial message"),
                    );
                }
            }
            push(&self.0, label);
            Ok(())
        }
    }
    struct Clock;
    impl TurnClock for Clock {
        fn now_iso(&self) -> JsString {
            "2026-01-01T00:00:00.000Z".into()
        }
        fn uuid(&self) -> JsString {
            "id".into()
        }
    }
    fn route() -> Route {
        Route {
            provider: "test".into(),
            model: "model".into(),
        }
    }
    fn execution() -> ExecutionHints {
        ExecutionHints {
            run_id: "run".into(),
            operation_id: "op".into(),
            idempotency_key: None,
            operation_deadline: None,
        }
    }
    fn message(text: &str) -> ProjectedMessage {
        let mut value = JsObject::new();
        value.insert("role".into(), "user".into());
        value.insert("id".into(), text.into());
        ProjectedMessage {
            boundary: BoundarySnapshot::capture(&JsValue::Object(value)),
            normalized: MessageProjection::BoundaryOnly(vec![
                crate::ProjectionGap::OtherBoundaryShape,
            ]),
        }
    }
    fn config() -> TurnRuntimeConfig {
        TurnRuntimeConfig {
            route: route(),
            cwd: "/controlled/project".into(),
            system_prompt: None,
            append_system_prompt: None,
            plan_mode_instructions: None,
            runtime_context_surface: RuntimeContextSurface::SystemPrompt,
            max_output_tokens: None,
            max_context_tokens: None,
            thinking: None,
            tool_choice: None,
            max_context_messages: None,
            stop_on_structured_output: None,
            json_self_correct: None,
            run_mode: None,
            is_subagent: None,
            subagent_model: None,
            permission_mode_before_plan: None,
            permission: TurnPermissionContext {
                mode: PermissionMode::Default,
                can_prompt: false,
                bypass_available: false,
                additional_working_directories: vec![],
                rules: None,
            },
            metadata: None,
        }
    }
    fn input() -> TurnInput {
        TurnInput {
            session_id: "session".into(),
            turn_id: "turn".into(),
            messages: vec![],
            max_turns: None,
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        }
    }
    fn tool_hints() -> ToolHints {
        ToolHints {
            session_id: "session".into(),
            turn_id: "turn".into(),
            cwd: "/controlled/project".into(),
            permission_mode: PermissionMode::Default,
            run_mode: None,
            can_prompt: false,
            can_elicit: None,
            additional_working_directories: vec![],
            context_extensions: JsObject::new(),
            execution: execution(),
        }
    }
    /// Which Original probe trigger the fake ports reproduce.
    #[derive(Clone, Copy)]
    enum Trigger {
        /// `pre_armed`: the abort is set before the loop top check.
        PreArmed,
        /// `in_prepare_for_model`: the abort fires inside request preparation.
        InPrepare,
        /// `stream_throw`: the abort fires and the iterator throws.
        StreamThrow,
        /// `stream_eos`: the abort fires and the iterator completes normally.
        StreamEos,
        DeadlineUnknown,
        DeadlineUnknownBadAssembly,
    }
    fn run_result(trigger: Trigger, reason: &'static str) -> (PortResult<TurnOutput>, Vec<String>) {
        let log: Log = Arc::default();
        let cancellation = Cancellation::default();
        let plan = match trigger {
            Trigger::PreArmed => {
                arm(&cancellation, reason);
                StreamPlan::Unreached
            }
            Trigger::InPrepare => StreamPlan::Unreached,
            Trigger::StreamThrow => StreamPlan::ThrowWhileAborted,
            Trigger::StreamEos => StreamPlan::CompleteThenAbort,
            Trigger::DeadlineUnknown => StreamPlan::DeadlineUnknown,
            Trigger::DeadlineUnknownBadAssembly => StreamPlan::DeadlineUnknownBadAssembly,
        };
        let mut host = Host {
            log: log.clone(),
            plan,
            cancellation: cancellation.clone(),
            reason,
            pulls: 0,
        };
        let mut preparation = Preparation {
            log: log.clone(),
            arm_in_prepare: matches!(trigger, Trigger::InPrepare)
                .then(|| (cancellation.clone(), reason)),
        };
        let mut events = Events(log.clone());
        let control = CallControl {
            cancellation,
            deadline: None,
        };
        let cleanup = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let mut config = config();
        let turn_input = input();
        let mut hints = tool_hints();
        let output = run_admitted_turn(
            &mut config,
            &turn_input,
            vec![message("prior"), message("prompt")],
            &mut host,
            &mut preparation,
            &mut events,
            &Clock,
            &control,
            &cleanup,
            &mut hints,
        );
        let observed = log.lock().unwrap().clone();
        (output, observed)
    }
    fn run(trigger: Trigger, reason: &'static str) -> (TurnOutput, Vec<String>) {
        let (result, log) = run_result(trigger, reason);
        let output = result.expect("owning cancellation completes as a known aborted turn");
        assert_eq!(output.result.kind, TurnResultKind::Aborted);
        assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
        (output, log)
    }
    #[test]
    fn deadline_unknown_with_partial_text_completes_turn_without_settling_model_call() {
        // TS publishes an aborted turn before its DEADLINE_EXCEEDED outer
        // result_unknown terminal. Completion must not erase the pending call.
        let (result, log) = run_result(Trigger::DeadlineUnknown, "unused");
        let Err(PortError::AcceptedUnknown {
            identity,
            last_applied_sequence,
        }) = result
        else {
            panic!("aborted completion must retain the accepted model uncertainty");
        };
        assert!(identity.request_id.equals_str("accepted-model"));
        assert_eq!(last_applied_sequence, Some(7.0));
        assert_eq!(
            log,
            vec![
                "prepare",
                "model_request_started",
                "host:stream_next:1",
                "model_event",
                "model_event",
                "host:stream_next:2",
                "project_assembly",
                "assistant_message",
                "host:capture_turn:3",
                "turn_completed",
            ]
        );
    }
    #[test]
    fn deadline_unknown_with_assembly_type_error_preserves_ts_no_completion_boundary() {
        let (result, log) = run_result(Trigger::DeadlineUnknownBadAssembly, "unused");
        let Err(PortError::AcceptedUnknown {
            identity,
            last_applied_sequence,
        }) = result
        else {
            panic!("assembly failure must retain the original accepted model uncertainty");
        };
        assert!(identity.request_id.equals_str("accepted-model"));
        assert_eq!(last_applied_sequence, Some(7.0));
        assert_eq!(
            log,
            vec![
                "prepare",
                "model_request_started",
                "host:stream_next:1",
                "model_event",
                "host:stream_next:2"
            ]
        );
    }
    fn status_count(log: &[String]) -> usize {
        log.iter()
            .filter(|entry| entry.as_str() == "agent_status:turn_aborted")
            .count()
    }
    fn assert_partial_assistant_kept(output: &TurnOutput) {
        // Both stream branches keep the safe partial text message (:861-873
        // and :917-929): durableMessageRoles ["assistant"] in the probe.
        assert_eq!(output.messages.len(), 3);
        assert_partial_text(
            output
                .result
                .final_message
                .as_ref()
                .expect("partial assistant message is the final message")
                .boundary
                .value(),
        );
    }
    fn assert_partial_text(message: &JsValue) {
        let text = message
            .get("content")
            .and_then(|content| match content {
                JsValue::Array(blocks) => blocks.first(),
                _ => None,
            })
            .and_then(|block| block.get("text"))
            .and_then(JsValue::as_string)
            .cloned()
            .expect("text block");
        assert!(text.equals_str("partial answer"));
    }

    /// Probe branch `during_stream_throw_parity_cancel` (turnAbortedStatusCount
    /// 0, eventTypes model_request_started, model_event*, assistant_message,
    /// turn_completed). Original :859-885.
    #[test]
    fn owning_stream_throw_while_aborted_omits_turn_aborted_status() {
        let (output, log) = run(Trigger::StreamThrow, "parity_cancel");
        assert_eq!(status_count(&log), 0, "{log:?}");
        assert_eq!(
            log,
            vec![
                "prepare",
                "model_request_started",
                "host:stream_next:1",
                "model_event",
                "model_event",
                "host:stream_next:2",
                "project_assembly",
                "assistant_message",
                "host:capture_turn:3",
                "turn_completed",
            ]
        );
        assert_partial_assistant_kept(&output);
    }
    /// Probe branch `during_stream_eos_parity_cancel` (turnAbortedStatusCount 1,
    /// eventTypes ..., assistant_message, agent_status:turn_aborted,
    /// turn_completed). Original :916-946 with shouldSurfaceAbortStatus true.
    /// The Rust owning path has no durable onAgentStatusMessage counterpart;
    /// the accepted agent_status event is the only status surface.
    #[test]
    fn owning_stream_completed_then_abort_emits_exactly_one_turn_aborted_status() {
        let (output, log) = run(Trigger::StreamEos, "parity_cancel");
        assert_eq!(status_count(&log), 1, "{log:?}");
        assert_eq!(
            log,
            vec![
                "prepare",
                "model_request_started",
                "host:stream_next:1",
                "model_event",
                "model_event",
                "host:stream_next:2",
                "model_event",
                "project_assembly",
                "assistant_message",
                "agent_status:turn_aborted",
                "host:capture_turn:3",
                "turn_completed",
            ]
        );
        assert_partial_assistant_kept(&output);
    }
    /// Probe branch `during_stream_eos_parity_cancel` timing with a reason that
    /// shouldSurfaceAbortStatus(:395) rejects: no status even though the
    /// iterator completed (:939-942 yields nothing).
    #[test]
    fn owning_stream_completed_with_non_surfaced_reason_omits_status() {
        for reason in ["compaction_persistence_failed", "user_stop"] {
            let (output, log) = run(Trigger::StreamEos, reason);
            assert_eq!(status_count(&log), 0, "{reason}: {log:?}");
            assert!(log.contains(&"assistant_message".to_string()));
            assert_eq!(log.last().map(String::as_str), Some("turn_completed"));
            assert_partial_assistant_kept(&output);
        }
    }
    /// Probe branch `loop_top_prearmed_parity_cancel` (turnAbortedStatusCount 1,
    /// modelRequests 0, eventTypes agent_status:turn_aborted, turn_completed).
    #[test]
    fn owning_loop_top_prearmed_cancel_emits_one_status_without_model_request() {
        let (output, log) = run(Trigger::PreArmed, "parity_cancel");
        assert_eq!(
            log,
            vec![
                "agent_status:turn_aborted",
                "host:capture_turn:2",
                "turn_completed"
            ]
        );
        assert_eq!(output.messages.len(), 2);
        assert!(output.result.final_message.is_none());
    }
    /// Probe branch `loop_top_prearmed_quiet_reason` (turnAbortedStatusCount 0,
    /// eventTypes turn_completed only).
    #[test]
    fn owning_loop_top_prearmed_quiet_reason_omits_status() {
        let (_, log) = run(Trigger::PreArmed, "user_stop");
        assert_eq!(log, vec!["host:capture_turn:2", "turn_completed"]);
    }
    /// Probe branch `post_create_model_request_parity_cancel`
    /// (turnAbortedStatusCount 1, modelRequests 0): the abort fires inside
    /// prepare_for_model and is observed before any model request starts.
    #[test]
    fn owning_abort_during_prepare_emits_one_status_without_model_request() {
        let (output, log) = run(Trigger::InPrepare, "parity_cancel");
        assert_eq!(
            log,
            vec![
                "prepare",
                "agent_status:turn_aborted",
                "host:capture_turn:2",
                "turn_completed"
            ]
        );
        assert_eq!(output.messages.len(), 2);
    }
    /// Probe branch `during_stream_throw_parity_cancel` timing with a reason
    /// that would be surfaced on the other branch (`timeout:run-1`): :859-885
    /// never consults createAbortStatus, so still no status.
    #[test]
    fn owning_stream_throw_with_surfaced_reason_still_omits_status() {
        let (_, log) = run(Trigger::StreamThrow, "timeout:run-1");
        assert_eq!(status_count(&log), 0, "{log:?}");
    }
}
#[cfg(test)]
mod owning_ordinary_recovery_tests {
    //! OWNING-path tests for Original AgentLoop.ts:1382-1441 (ordinary model
    //! error with assembled tool calls). Each test drives `run_admitted_turn`
    //! through in-memory ports with the SAME ten controlled logical profiles
    //! the genuine Original (b243fa0) ran in
    //! evidence/CORE01-ordinary-recovery-original-coordinator-a3 (profiles.json,
    //! original-full-loop-probe.mjs). Expectations are the decoded Original
    //! observations translated to the Rust port protocol:
    //! - Original model.stream() invocation  -> one `stream_next` pull returning
    //!   the whole controlled batch (`done:true`; a source `error` event closes
    //!   the iterator exactly as in the Original).
    //! - Original tool port executeAll        -> HostCall::ExecuteBatch (counted;
    //!   ordinary profiles fail the batch, host_unknown replies AcceptedUnknown).
    //! - Original context.recoverFromModelError give_up -> RecoverModelErrorBoundary
    //!   replying `{type:"give_up"}`; captureTurn -> CaptureTurn (counted).
    //! - Original `onDurableMessage` is NOT called by the Rust engine. The TS
    //!   sidecar client invokes it after the `assistant_message` /
    //!   `tool_results_projected` events. The in-memory TurnEventPort therefore
    //!   models only the ENGINE translation of a durable failure: the port
    //!   returns Err on that emission and the engine must stop (no further pull,
    //!   dispatch, capture or event) preserving the error class. This is an
    //!   engine-translation claim, not a proof of the production client timing.
    use super::*;
    use crate::ports::{
        Cancellation, ExecutionHints, ExecutionIdentity, RequestProjection, Route,
        RuntimeContextSurface, StreamChunk, StreamEvent,
    };
    use crate::{ToolContent, ToolExecutionResult, ToolResultDisposition};
    use pilotdeck_protocol::model::{PreparedWireRequest, ProviderEcho, ToolCall};
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;
    fn push(log: &Log, entry: impl Into<String>) {
        log.lock().unwrap().push(entry.into());
    }
    fn json(text: &str) -> JsValue {
        pilotdeck_protocol::parse_json(text).unwrap()
    }
    fn raw_event(text: &str) -> StreamEvent {
        StreamEvent::BoundaryOnly(BoundarySnapshot::capture(&json(text)))
    }
    fn str_of(value: &JsValue, key: &str) -> Option<String> {
        value
            .get(key)
            .and_then(JsValue::as_string)
            .map(|s| s.to_utf8().unwrap())
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum DurableReject {
        None,
        Assistant,
        Pair,
    }
    /// One entry of the Original run's profiles.json (same ids, call ids,
    /// names, inputs, codes and flags).
    struct Profile {
        id: &'static str,
        error_attempts: usize,
        calls: usize,
        json_self_correct: bool,
        error_code: &'static str,
        reject_durable: DurableReject,
        callback_unknown: bool,
        host_unknown: bool,
    }
    const PROFILES: [Profile; 10] = [
        Profile {
            id: "ordinary-original-a1-one-completed-call",
            error_attempts: 1,
            calls: 1,
            json_self_correct: false,
            error_code: "ordinary_fixture_failure",
            reject_durable: DurableReject::None,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-two-completed-calls",
            error_attempts: 1,
            calls: 2,
            json_self_correct: false,
            error_code: "ordinary_fixture_failure",
            reject_durable: DurableReject::None,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-json-three-then-success",
            error_attempts: 3,
            calls: 1,
            json_self_correct: true,
            error_code: "invalid_tool_arguments",
            reject_durable: DurableReject::None,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-json-four-errors",
            error_attempts: 4,
            calls: 1,
            json_self_correct: true,
            error_code: "invalid_tool_arguments",
            reject_durable: DurableReject::None,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-json-disabled",
            error_attempts: 1,
            calls: 1,
            json_self_correct: false,
            error_code: "invalid_tool_arguments",
            reject_durable: DurableReject::None,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-json-other-code",
            error_attempts: 1,
            calls: 1,
            json_self_correct: true,
            error_code: "other_fixture_error",
            reject_durable: DurableReject::None,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-durable-assistant-reject",
            error_attempts: 1,
            calls: 1,
            json_self_correct: true,
            error_code: "invalid_tool_arguments",
            reject_durable: DurableReject::Assistant,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-durable-pair-reject",
            error_attempts: 1,
            calls: 2,
            json_self_correct: true,
            error_code: "invalid_tool_arguments",
            reject_durable: DurableReject::Pair,
            callback_unknown: false,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-durable-pair-unknown",
            error_attempts: 1,
            calls: 2,
            json_self_correct: true,
            error_code: "invalid_tool_arguments",
            reject_durable: DurableReject::Pair,
            callback_unknown: true,
            host_unknown: false,
        },
        Profile {
            id: "ordinary-original-a1-host-tool-result-unknown",
            error_attempts: 0,
            calls: 1,
            json_self_correct: false,
            error_code: "ordinary_fixture_failure",
            reject_durable: DurableReject::None,
            callback_unknown: false,
            host_unknown: true,
        },
    ];
    const SUCCESS_TEXT: &str = "controlled success";
    const MISSING_RESULT_TEXT: &str = "TOOL_ERROR[tool_execution_failed][owned_memory_tool][environment_issue]\nSummary: Model error interrupted tool execution.\nEvidence:\n- Model error interrupted tool execution.\nOriginal error:\nModel error interrupted tool execution.\nNext actions:\n1. Inspect the evidence, change the approach, then retry only with corrected inputs. Current permission mode: bypassPermissions.";
    const JSON_PROMPT: &str = "Your previous tool call contained invalid JSON in the arguments and could not be parsed. Please retry with valid JSON. Common issues: missing quotes around keys/values, trailing commas, unescaped special characters in strings.";
    const NOW: &str = "2026-10-10T00:00:00.000Z";

    fn call_id(p: &Profile, ordinal: usize) -> String {
        format!("{}-call-{}", p.id, ordinal + 1)
    }
    /// The probe's controlled `stream()` for invocation `number` (1-based).
    fn controlled_stream(p: &Profile, number: usize) -> Vec<StreamEvent> {
        let mut events = vec![raw_event(r#"{"type":"message_start","role":"assistant"}"#)];
        let failure = number <= p.error_attempts;
        if failure || p.host_unknown {
            for ordinal in 0..p.calls {
                let id = call_id(p, ordinal);
                events.push(raw_event(&format!(
                    r#"{{"type":"tool_call_start","id":"{id}","name":"owned_memory_tool"}}"#
                )));
                events.push(raw_event(&format!(
                    r#"{{"type":"tool_call_end","toolCall":{{"id":"{id}","name":"owned_memory_tool","input":{{"ordinal":{ordinal}}},"raw":{{"controlledSource":"complete provider event"}}}}}}"#
                )));
            }
            if failure {
                events.push(raw_event(&format!(
                    r#"{{"type":"error","error":{{"provider":"fixture-provider","protocol":"openai","code":"{}","message":"Controlled ordinary model error","retryable":false}}}}"#,
                    p.error_code
                )));
            } else {
                events.push(raw_event(
                    r#"{"type":"message_end","finishReason":"tool_calls"}"#,
                ));
            }
        } else {
            events.push(raw_event(&format!(
                r#"{{"type":"text_delta","text":"{SUCCESS_TEXT}"}}"#
            )));
            events.push(raw_event(r#"{"type":"message_end","finishReason":"stop"}"#));
        }
        events
    }
    fn identity(p: &Profile, request_id: &str) -> ExecutionIdentity {
        ExecutionIdentity {
            session_id: format!("{}-session", p.id).into(),
            turn_id: format!("{}-turn", p.id).into(),
            run_id: "run".into(),
            operation_id: "op".into(),
            request_id: request_id.into(),
            idempotency_key: None,
            module_instance_id: "controlled-module".into(),
            connection_generation: "generation-1".into(),
        }
    }
    fn host_unknown_identity(p: &Profile) -> ExecutionIdentity {
        identity(p, &format!("tool-{}-host-2", p.id))
    }
    fn durable_unknown_identity(p: &Profile) -> ExecutionIdentity {
        identity(p, &format!("{}-durable-2", p.id))
    }
    struct Host {
        profile: &'static Profile,
        log: Log,
        pulls: usize,
        dispatches: usize,
        declines: usize,
        captures: Vec<(bool, usize)>,
    }
    impl HostPort for Host {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            matches!(
                (module, operation),
                ("model", "stream_next")
                    | ("context", "capture_turn")
                    | ("context", "recover_from_model_error")
                    | ("capability", "execute_batch")
            )
        }
        fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
            assert!(control.check().is_ok(), "no host call after cancellation");
            match call {
                HostCall::ModelPull { .. } => {
                    self.pulls += 1;
                    assert!(self.pulls <= 5, "Model bound5 (Original probe bound)");
                    push(&self.log, format!("host:stream_next:{}", self.pulls));
                    Ok(HostReply::ModelChunk(StreamChunk {
                        events: controlled_stream(self.profile, self.pulls),
                        done: true,
                    }))
                }
                HostCall::ExecuteBatch { calls, .. } => {
                    self.dispatches += 1;
                    push(&self.log, format!("host:execute_batch:{}", calls.len()));
                    if self.profile.host_unknown {
                        assert_eq!(self.dispatches, 1, "No Unknown retry");
                        assert_eq!(calls.len(), 1);
                        assert!(calls[0].tool_call_id.equals_str(&call_id(self.profile, 0)));
                        Err(PortError::AcceptedUnknown {
                            identity: Box::new(host_unknown_identity(self.profile)),
                            last_applied_sequence: Some(1.0),
                        })
                    } else {
                        // Original probe: executeAll throws for ordinary profiles.
                        push(&self.log, "UNEXPECTED_TOOL_DISPATCH");
                        Err(PortError::HostFailure {
                            code: "controlled_fixture".into(),
                            message: "Ordinary-error profile cannot execute tools".into(),
                            boundary: BoundarySnapshot::capture(&JsValue::Null),
                        })
                    }
                }
                HostCall::RecoverModelErrorBoundary(probe) => {
                    self.declines += 1;
                    assert!(!probe.has_attempted_compact);
                    assert_eq!(
                        str_of(probe.error.value(), "code").as_deref(),
                        Some(self.profile.error_code)
                    );
                    push(&self.log, "host:recover_from_model_error:give_up");
                    Ok(HostReply::RecoveryBoundaryOnly(BoundarySnapshot::capture(
                        &json(r#"{"type":"give_up","reason":"bounded fixture decline"}"#),
                    )))
                }
                HostCall::CaptureTurn(capture) => {
                    self.captures
                        .push((capture.errored, capture.messages.len()));
                    push(
                        &self.log,
                        format!(
                            "host:capture_turn:{}:{}",
                            if capture.errored { "errored" } else { "ok" },
                            capture.messages.len()
                        ),
                    );
                    Ok(HostReply::CapturedTurn)
                }
                other => panic!("unexpected host call {:?}", other.method()),
            }
        }
    }
    struct Preparation {
        log: Log,
        prepares: Vec<Vec<BoundarySnapshot>>,
    }
    impl TurnPreparationPort for Preparation {
        fn prepare(
            &mut self,
            _: &mut dyn HostPort,
            _: &CallControl,
            _: &TurnRuntimeConfig,
            _: &TurnInput,
            messages: &[ProjectedMessage],
        ) -> PortResult<(CapturedRequest, ModelHints)> {
            push(&self.log, format!("prepare:{}", messages.len()));
            self.prepares
                .push(messages.iter().map(|m| m.boundary.clone()).collect());
            Ok((
                CapturedRequest {
                    route: route(),
                    normalized: RequestProjection::BoundaryOnly(Vec::new()),
                    wire: PreparedWireRequest::capture(&json(
                        r#"{"provider":"fixture-provider","model":"fixture-model","messages":[]}"#,
                    )),
                },
                ModelHints {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    execution: ExecutionHints {
                        run_id: "run".into(),
                        operation_id: "op".into(),
                        idempotency_key: None,
                        operation_deadline: None,
                    },
                    workspace_id: None,
                    storage_config_version: None,
                    metadata: None,
                    model_override: None,
                },
            ))
        }
        fn project_assembly(&self, boundary: &BoundarySnapshot) -> PortResult<TurnAssembly> {
            push(&self.log, "project_assembly");
            let message = boundary
                .value()
                .get("message")
                .cloned()
                .expect("assembled message");
            Ok(TurnAssembly {
                boundary: boundary.clone(),
                message: ProjectedMessage {
                    boundary: BoundarySnapshot::capture(&message),
                    normalized: MessageProjection::BoundaryOnly(vec![
                        crate::ProjectionGap::OtherBoundaryShape,
                    ]),
                },
                usage: None,
                has_repaired_tool_calls: None,
                has_message_end: boundary.value().get("hasMessageEnd")
                    == Some(&JsValue::Bool(true)),
            })
        }
        fn project_tool_calls(&self, calls: &[BoundarySnapshot]) -> PortResult<Vec<ToolCall>> {
            Ok(calls
                .iter()
                .map(|call| {
                    let value = call.value();
                    ToolCall {
                        timeline: None,
                        id: value
                            .get("id")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .expect("id"),
                        name: value
                            .get("name")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .expect("name"),
                        input: value.get("input").cloned().unwrap_or(JsValue::Undefined),
                        raw: value
                            .get("raw")
                            .map(|raw| ProviderEcho(Arc::new(raw.clone()))),
                    }
                })
                .collect())
        }
        /// Typed admission of the complete createMissingToolResult object (the
        /// production factory uses model_admission::tool_result for the same).
        fn project_tool_result(
            &self,
            result: &BoundarySnapshot,
        ) -> PortResult<ToolExecutionResult> {
            let value = result.value();
            let text = |key: &str| -> JsString {
                value
                    .get(key)
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_else(|| panic!("missing-result string field {key}"))
            };
            let error = value.get("error").expect("error object");
            let JsValue::Array(blocks) = value.get("content").expect("content") else {
                panic!("content array");
            };
            Ok(ToolExecutionResult {
                tool_call_id: text("toolCallId"),
                tool_name: text("toolName"),
                disposition: ToolResultDisposition::Error {
                    code: error
                        .get("code")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap(),
                    message: error
                        .get("message")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap(),
                    fields: JsObject::new(),
                },
                content: blocks
                    .iter()
                    .map(|block| ToolContent::Text {
                        text: block
                            .get("text")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap(),
                        boundary: BoundarySnapshot::capture(block),
                    })
                    .collect(),
                supplemental_messages: None,
                metadata: value.get("metadata").and_then(JsValue::as_object).cloned(),
                started_at: text("startedAt"),
                completed_at: text("completedAt"),
                boundary: result.clone(),
            })
        }
    }
    /// Event sink. `fail_on` models the engine-side translation of a durable
    /// failure: the port rejects exactly one emission (by label) with the
    /// configured error, as the Original's `onDurableMessage` threw once.
    struct Events {
        log: Log,
        events: Vec<BoundarySnapshot>,
        fail_on: Option<(&'static str, PortError)>,
    }
    impl Events {
        fn label(event: &BoundarySnapshot) -> String {
            let value = event.value();
            let kind = str_of(value, "type").unwrap();
            match kind.as_str() {
                "agent_status" => format!("agent_status:{}", str_of(value, "event").unwrap()),
                "turn_continued" => format!("turn_continued:{}", str_of(value, "reason").unwrap()),
                _ => kind,
            }
        }
    }
    impl TurnEventPort for Events {
        fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            let label = Self::label(&event);
            self.events.push(event);
            if self
                .fail_on
                .as_ref()
                .is_some_and(|(target, _)| *target == label)
            {
                let (_, error) = self.fail_on.take().unwrap();
                push(&self.log, format!("{label}:REJECTED"));
                return Err(error);
            }
            push(&self.log, label);
            Ok(())
        }
    }
    struct Clock {
        id: &'static str,
        uuids: Cell<usize>,
    }
    impl TurnClock for Clock {
        fn now_iso(&self) -> JsString {
            NOW.into()
        }
        fn uuid(&self) -> JsString {
            self.uuids.set(self.uuids.get() + 1);
            format!("{}-uuid-{}", self.id, self.uuids.get()).into()
        }
    }
    fn route() -> Route {
        Route {
            provider: "fixture-provider".into(),
            model: "fixture-model".into(),
        }
    }
    fn config(p: &Profile) -> TurnRuntimeConfig {
        TurnRuntimeConfig {
            route: route(),
            cwd: "/controlled-workspace".into(),
            system_prompt: None,
            append_system_prompt: None,
            plan_mode_instructions: None,
            runtime_context_surface: RuntimeContextSurface::SystemPrompt,
            max_output_tokens: None,
            max_context_tokens: Some(32768.0),
            thinking: None,
            tool_choice: None,
            max_context_messages: None,
            stop_on_structured_output: None,
            json_self_correct: Some(p.json_self_correct),
            run_mode: None,
            is_subagent: None,
            subagent_model: None,
            permission_mode_before_plan: None,
            permission: TurnPermissionContext {
                mode: PermissionMode::BypassPermissions,
                can_prompt: false,
                bypass_available: true,
                additional_working_directories: vec![],
                rules: None,
            },
            metadata: None,
        }
    }
    fn input(p: &Profile) -> TurnInput {
        TurnInput {
            session_id: format!("{}-session", p.id).into(),
            turn_id: format!("{}-turn", p.id).into(),
            messages: vec![],
            max_turns: Some(8.0),
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        }
    }
    fn tool_hints(p: &Profile) -> ToolHints {
        ToolHints {
            session_id: format!("{}-session", p.id).into(),
            turn_id: format!("{}-turn", p.id).into(),
            cwd: "/controlled-workspace".into(),
            permission_mode: PermissionMode::BypassPermissions,
            run_mode: None,
            can_prompt: false,
            can_elicit: None,
            additional_working_directories: vec![],
            context_extensions: JsObject::new(),
            execution: ExecutionHints {
                run_id: "run".into(),
                operation_id: "op".into(),
                idempotency_key: None,
                operation_deadline: None,
            },
        }
    }
    fn initial_message() -> ProjectedMessage {
        ProjectedMessage {
            boundary: BoundarySnapshot::capture(&json(
                r#"{"role":"user","content":[{"type":"text","text":"Controlled ordinary recovery input"}]}"#,
            )),
            normalized: MessageProjection::BoundaryOnly(vec![
                crate::ProjectionGap::OtherBoundaryShape,
            ]),
        }
    }
    struct Observed {
        result: PortResult<TurnOutput>,
        log: Vec<String>,
        events: Vec<BoundarySnapshot>,
        pulls: usize,
        dispatches: usize,
        declines: usize,
        captures: Vec<(bool, usize)>,
        prepares: Vec<Vec<BoundarySnapshot>>,
    }
    impl Observed {
        fn count(&self, label: &str) -> usize {
            self.log
                .iter()
                .filter(|entry| entry.as_str() == label)
                .count()
        }
        fn events_of(&self, kind: &str) -> Vec<&JsValue> {
            self.events
                .iter()
                .map(BoundarySnapshot::value)
                .filter(|value| str_of(value, "type").as_deref() == Some(kind))
                .collect()
        }
        /// Rust counterpart of the Original `durables` counter: every attempted
        /// `assistant_message` / `tool_results_projected` emission.
        fn durable_equivalent_emissions(&self) -> usize {
            self.events_of("assistant_message").len()
                + self.events_of("tool_results_projected").len()
        }
        fn terminal(&self) -> String {
            match &self.result {
                Ok(output) => format!(
                    "returned:{:?}/{:?}",
                    output.result.kind, output.result.stop_reason
                ),
                Err(PortError::AcceptedUnknown { identity, .. }) => {
                    format!(
                        "err:AcceptedUnknown:{}",
                        identity.request_id.to_utf8().unwrap()
                    )
                }
                Err(PortError::HostFailure { message, .. }) => {
                    format!("err:HostFailure:{}", message.to_utf8().unwrap())
                }
                Err(other) => format!("err:{other:?}"),
            }
        }
    }
    fn run(p: &'static Profile) -> Observed {
        let log: Log = Arc::default();
        let mut host = Host {
            profile: p,
            log: log.clone(),
            pulls: 0,
            dispatches: 0,
            declines: 0,
            captures: vec![],
        };
        let mut preparation = Preparation {
            log: log.clone(),
            prepares: vec![],
        };
        let rejection = || PortError::HostFailure {
            code: "controlled_durable_rejection".into(),
            message: "Controlled durable callback rejection".into(),
            boundary: BoundarySnapshot::capture(&JsValue::Null),
        };
        let fail_on = match (p.reject_durable, p.callback_unknown) {
            (DurableReject::None, _) => None,
            (DurableReject::Assistant, false) => Some(("assistant_message", rejection())),
            (DurableReject::Pair, false) => Some(("tool_results_projected", rejection())),
            (DurableReject::Assistant, true) => unreachable!("not an Original profile"),
            (DurableReject::Pair, true) => Some((
                "tool_results_projected",
                PortError::AcceptedUnknown {
                    identity: Box::new(durable_unknown_identity(p)),
                    last_applied_sequence: Some(2.0),
                },
            )),
        };
        let mut events = Events {
            log: log.clone(),
            events: vec![],
            fail_on,
        };
        let clock = Clock {
            id: p.id,
            uuids: Cell::new(0),
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let cleanup = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let mut config = config(p);
        let turn_input = input(p);
        let mut hints = tool_hints(p);
        let result = run_admitted_turn(
            &mut config,
            &turn_input,
            vec![initial_message()],
            &mut host,
            &mut preparation,
            &mut events,
            &clock,
            &control,
            &cleanup,
            &mut hints,
        );
        let observed = Observed {
            result,
            log: log.lock().unwrap().clone(),
            events: events.events,
            pulls: host.pulls,
            dispatches: host.dispatches,
            declines: host.declines,
            captures: host.captures,
            prepares: preparation.prepares,
        };
        println!(
            "RUST_OBSERVED {{\"case_id\":\"{}\",\"models\":{},\"toolDispatches\":{},\"durableEquivalentEmissions\":{},\"captures\":{},\"turn_continued\":{},\"recovery_declines\":{},\"terminal\":\"{}\",\"log\":[{}]}}",
            p.id,
            observed.pulls,
            observed.dispatches,
            observed.durable_equivalent_emissions(),
            observed.captures.len(),
            observed.count("turn_continued:model_error"),
            observed.declines,
            observed.terminal(),
            observed
                .log
                .iter()
                .map(|entry| format!("\"{entry}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        observed
    }
    /// Expected Rust log for a profile, derived from the Original public-event
    /// order (model_request_started, model_event*, assistant_message,
    /// tool_results_projected, turn_continued | terminal) plus the host calls
    /// the Original recorded (context_recovery_decline, context_capture).
    fn expected_log(p: &Profile) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        let mut messages = 1usize;
        let mut pending_transient = false;
        let model_events_errored = 2 + 2 * p.calls;
        for n in 1..=p.error_attempts {
            out.push(format!(
                "prepare:{}",
                messages + usize::from(pending_transient)
            ));
            out.push("model_request_started".into());
            out.push(format!("host:stream_next:{n}"));
            out.extend(std::iter::repeat_n(
                "model_event".to_string(),
                model_events_errored,
            ));
            out.push("project_assembly".into());
            if p.reject_durable == DurableReject::Assistant {
                out.push("assistant_message:REJECTED".into());
                return out;
            }
            out.push("assistant_message".into());
            if p.reject_durable == DurableReject::Pair {
                out.push("tool_results_projected:REJECTED".into());
                return out;
            }
            out.push("tool_results_projected".into());
            messages += 2;
            if p.json_self_correct && p.error_code == "invalid_tool_arguments" && n <= 3 {
                out.push("turn_continued:model_error".into());
                pending_transient = true;
                continue;
            }
            out.push("host:recover_from_model_error:give_up".into());
            out.push("stop_failure".into());
            out.push("agent_status:model_request_failed".into());
            out.push("turn_failed".into());
            out.push(format!("host:capture_turn:errored:{messages}"));
            out.push("turn_completed".into());
            return out;
        }
        let n = p.error_attempts + 1;
        out.push(format!(
            "prepare:{}",
            messages + usize::from(pending_transient)
        ));
        out.push("model_request_started".into());
        out.push(format!("host:stream_next:{n}"));
        if p.host_unknown {
            out.extend(std::iter::repeat_n(
                "model_event".to_string(),
                2 + 2 * p.calls,
            ));
            out.push("project_assembly".into());
            out.push("assistant_message".into());
            out.push("tool_calls_detected".into());
            out.push("host:execute_batch:1".into());
            return out;
        }
        out.extend(std::iter::repeat_n("model_event".to_string(), 3));
        out.push("project_assembly".into());
        out.push("assistant_message".into());
        out.push("stop_requested".into());
        out.push(format!("host:capture_turn:ok:{}", messages + 1));
        out.push("turn_completed".into());
        out
    }
    /// (block type, toolCallId, isError) of one wire content block.
    type BlockShape = (String, Option<String>, Option<bool>);
    /// (role, blocks) of a wire message.
    fn shape(message: &JsValue) -> (String, Vec<BlockShape>) {
        let JsValue::Array(blocks) = message.get("content").expect("content") else {
            panic!("content array");
        };
        (
            str_of(message, "role").unwrap(),
            blocks
                .iter()
                .map(|block| {
                    (
                        str_of(block, "type").unwrap(),
                        str_of(block, "toolCallId"),
                        block.get("isError").and_then(JsValue::as_bool),
                    )
                })
                .collect(),
        )
    }
    fn assert_projected_pair(p: &Profile, message: &JsValue) {
        let (role, blocks) = shape(message);
        assert_eq!(role, "user");
        assert_eq!(blocks.len(), p.calls, "one tool_result per assembled call");
        for (ordinal, (kind, id, is_error)) in blocks.iter().enumerate() {
            assert_eq!(kind, "tool_result");
            assert_eq!(id.as_deref(), Some(call_id(p, ordinal).as_str()));
            assert_eq!(*is_error, Some(true));
        }
        let JsValue::Array(blocks) = message.get("content").unwrap() else {
            unreachable!()
        };
        for block in blocks {
            let JsValue::Array(inner) = block.get("content").expect("tool_result content") else {
                panic!("inner content array");
            };
            assert_eq!(inner.len(), 1);
            assert_eq!(
                str_of(&inner[0], "text").as_deref(),
                Some(MISSING_RESULT_TEXT)
            );
            let raw = block.get("raw").expect("raw missing result");
            assert_eq!(str_of(raw, "type").as_deref(), Some("error"));
            assert_eq!(
                str_of(raw, "toolName").as_deref(),
                Some("owned_memory_tool")
            );
            assert_eq!(str_of(raw, "startedAt").as_deref(), Some(NOW));
            assert_eq!(str_of(raw, "completedAt").as_deref(), Some(NOW));
            assert_eq!(
                str_of(raw.get("error").unwrap(), "message").as_deref(),
                Some("Model error interrupted tool execution.")
            );
        }
    }
    fn assert_common(p: &'static Profile, observed: &Observed) {
        assert_eq!(
            observed.log,
            expected_log(p),
            "{}: {:?}",
            p.id,
            observed.log
        );
        // Every assistant message carries the assembled calls (or the success text).
        for message in observed.events_of("assistant_message") {
            let message = message.get("message").unwrap();
            let (role, blocks) = shape(message);
            assert_eq!(role, "assistant");
            if blocks.iter().all(|(kind, _, _)| kind == "text") {
                assert_eq!(
                    str_of(&message.get("content").unwrap().as_array_ref()[0], "text").as_deref(),
                    Some(SUCCESS_TEXT)
                );
            } else {
                assert_eq!(blocks.len(), p.calls);
                for (ordinal, (kind, _, _)) in blocks.iter().enumerate() {
                    assert_eq!(kind, "tool_call");
                    let block = &message.get("content").unwrap().as_array_ref()[ordinal];
                    assert_eq!(
                        str_of(block, "id").as_deref(),
                        Some(call_id(p, ordinal).as_str())
                    );
                    assert_eq!(str_of(block, "name").as_deref(), Some("owned_memory_tool"));
                }
            }
        }
        for projected in observed.events_of("tool_results_projected") {
            assert_projected_pair(p, projected.get("message").unwrap());
        }
        // The engine never pulls the model more times than the Original did.
        assert_eq!(observed.pulls, observed.count("model_request_started"));
    }
    trait ArrayRef {
        fn as_array_ref(&self) -> &[JsValue];
    }
    impl ArrayRef for JsValue {
        fn as_array_ref(&self) -> &[JsValue] {
            match self {
                JsValue::Array(items) => items,
                _ => panic!("array"),
            }
        }
    }
    fn assert_terminal_model_error(p: &Profile, observed: &Observed, message_count: usize) {
        let output = observed
            .result
            .as_ref()
            .expect("ordinary model error returns a result");
        assert_eq!(output.result.kind, TurnResultKind::Error);
        assert_eq!(output.result.stop_reason, TurnStopReason::ModelError);
        assert_eq!(output.result.turns, 1.0);
        assert_eq!(output.result.errors.len(), 1);
        let error = output.result.errors[0].value();
        assert_eq!(str_of(error, "code").as_deref(), Some("agent_model_error"));
        assert_eq!(
            str_of(error, "message").as_deref(),
            Some("Controlled ordinary model error")
        );
        assert_eq!(
            str_of(error.get("details").unwrap(), "code").as_deref(),
            Some(p.error_code)
        );
        assert_eq!(output.messages.len(), message_count);
        let roles: Vec<String> = output
            .messages
            .iter()
            .map(|m| str_of(m.boundary.value(), "role").unwrap())
            .collect();
        let mut expected_roles = vec!["user".to_string()];
        for _ in 0..(message_count - 1) / 2 {
            expected_roles.push("assistant".into());
            expected_roles.push("user".into());
        }
        assert_eq!(roles, expected_roles);
        // No transient prompt survives into the returned/captured history.
        assert!(output.messages.iter().all(|m| {
            m.boundary
                .value()
                .get("metadata")
                .and_then(|m| m.get("transient"))
                .is_none()
        }));
        let final_message = output
            .result
            .final_message
            .as_ref()
            .expect("final assistant");
        assert_eq!(shape(final_message.boundary.value()).0, "assistant");
        assert_eq!(
            observed.declines, 1,
            "one reactive recovery probe (give_up)"
        );
        assert_eq!(observed.captures, vec![(true, message_count)]);
    }
    fn assert_transient_prompt_fed_back(p: &Profile, observed: &Observed) {
        // Original context_prepare 2..4: the last message of every retry
        // request is the transient json_self_correct prompt; it is expired
        // before the next assistant is appended (returned history has none).
        let retries = observed.prepares.len() - 1;
        assert_eq!(retries, 3);
        for (index, messages) in observed.prepares.iter().enumerate().skip(1) {
            let last = messages.last().unwrap().value();
            assert_eq!(str_of(last, "role").as_deref(), Some("user"));
            let metadata = last.get("metadata").expect("transient metadata");
            assert_eq!(metadata.get("synthetic"), Some(&JsValue::Bool(true)));
            assert_eq!(metadata.get("transient"), Some(&JsValue::Bool(true)));
            assert_eq!(
                str_of(metadata, "purpose").as_deref(),
                Some("json_self_correct")
            );
            assert!(str_of(metadata, "transientId").is_some());
            assert_eq!(
                str_of(&last.get("content").unwrap().as_array_ref()[0], "text").as_deref(),
                Some(JSON_PROMPT)
            );
            assert_eq!(
                messages.len(),
                2 * (index + 1),
                "{}: prepare {}",
                p.id,
                index + 1
            );
            let transient_count = messages
                .iter()
                .filter(|m| {
                    m.value()
                        .get("metadata")
                        .and_then(|m| m.get("transient"))
                        .is_some()
                })
                .count();
            assert_eq!(
                transient_count, 1,
                "only the newest transient prompt is live"
            );
        }
    }

    /// Profile 1 (Original: models 1, durables 2, captures 1, decline 1,
    /// returned error/model_error, 3 messages).
    #[test]
    fn profile_1_one_completed_call_projects_missing_result_then_terminal_error() {
        let p = &PROFILES[0];
        let observed = run(p);
        assert_common(p, &observed);
        assert_eq!((observed.pulls, observed.dispatches), (1, 0));
        assert_eq!(observed.durable_equivalent_emissions(), 2);
        assert_eq!(observed.count("turn_continued:model_error"), 0);
        assert_terminal_model_error(p, &observed, 3);
    }
    /// Profile 2 (Original: one projected user message holding BOTH results,
    /// durables 2, 3 messages).
    #[test]
    fn profile_2_two_completed_calls_project_one_message_with_two_results() {
        let p = &PROFILES[1];
        let observed = run(p);
        assert_common(p, &observed);
        assert_eq!((observed.pulls, observed.dispatches), (1, 0));
        assert_eq!(observed.durable_equivalent_emissions(), 2);
        assert_eq!(observed.events_of("tool_results_projected").len(), 1);
        assert_terminal_model_error(p, &observed, 3);
    }
    /// Profile 3 (Original: models 4, durables 7, turn_continued x3, captures 1
    /// errored=false, returned success/completed, 8 messages).
    #[test]
    fn profile_3_json_three_errors_then_success_on_fourth_model_call() {
        let p = &PROFILES[2];
        let observed = run(p);
        assert_common(p, &observed);
        assert_eq!((observed.pulls, observed.dispatches), (4, 0));
        assert_eq!(observed.durable_equivalent_emissions(), 7);
        assert_eq!(observed.count("turn_continued:model_error"), 3);
        assert_eq!(observed.declines, 0);
        assert_transient_prompt_fed_back(p, &observed);
        let output = observed.result.as_ref().expect("success");
        assert_eq!(output.result.kind, TurnResultKind::Success);
        assert_eq!(output.result.stop_reason, TurnStopReason::Completed);
        assert_eq!(output.messages.len(), 8);
        assert_eq!(observed.captures, vec![(false, 8)]);
        let final_message = output
            .result
            .final_message
            .as_ref()
            .unwrap()
            .boundary
            .value();
        assert_eq!(
            str_of(
                &final_message.get("content").unwrap().as_array_ref()[0],
                "text"
            )
            .as_deref(),
            Some(SUCCESS_TEXT)
        );
    }
    /// Profile 4 (Original: models 4 - the 4th error is NOT retried -, durables
    /// 8, turn_continued x3, decline 1, returned error/model_error, 9 messages).
    #[test]
    fn profile_4_json_fourth_error_exhausts_three_corrections_and_fails() {
        let p = &PROFILES[3];
        let observed = run(p);
        assert_common(p, &observed);
        assert_eq!((observed.pulls, observed.dispatches), (4, 0));
        assert_eq!(observed.durable_equivalent_emissions(), 8);
        assert_eq!(observed.count("turn_continued:model_error"), 3);
        assert_transient_prompt_fed_back(p, &observed);
        assert_terminal_model_error(p, &observed, 9);
    }
    /// Profile 5 (jsonSelfCorrect disabled, code invalid_tool_arguments):
    /// Original models 1, no turn_continued.
    #[test]
    fn profile_5_json_disabled_does_not_retry() {
        let p = &PROFILES[4];
        let observed = run(p);
        assert_common(p, &observed);
        assert_eq!((observed.pulls, observed.dispatches), (1, 0));
        assert_eq!(observed.count("turn_continued:model_error"), 0);
        assert_eq!(observed.durable_equivalent_emissions(), 2);
        assert_terminal_model_error(p, &observed, 3);
    }
    /// Profile 6 (jsonSelfCorrect enabled, other code): Original models 1.
    #[test]
    fn profile_6_json_other_code_does_not_retry() {
        let p = &PROFILES[5];
        let observed = run(p);
        assert_common(p, &observed);
        assert_eq!((observed.pulls, observed.dispatches), (1, 0));
        assert_eq!(observed.count("turn_continued:model_error"), 0);
        assert_eq!(observed.durable_equivalent_emissions(), 2);
        assert_terminal_model_error(p, &observed, 3);
    }
    fn assert_durable_failure_stops_engine(
        p: &'static Profile,
        observed: &Observed,
        emissions: usize,
    ) {
        assert_common(p, observed);
        assert_eq!((observed.pulls, observed.dispatches), (1, 0));
        assert_eq!(
            observed.declines, 0,
            "no reactive probe after the durable failure"
        );
        assert!(
            observed.captures.is_empty(),
            "no capture_turn after the durable failure"
        );
        assert_eq!(observed.durable_equivalent_emissions(), emissions);
        assert!(
            observed.log.last().unwrap().ends_with(":REJECTED"),
            "{:?}",
            observed.log
        );
        assert_eq!(observed.count("turn_continued:model_error"), 0);
    }
    /// Profile 7 (Original: onDurableMessage threw on the assistant; models 1,
    /// durables 1, captures 0, run threw the same plain Error).
    #[test]
    fn profile_7_durable_assistant_rejection_stops_before_projection() {
        let p = &PROFILES[6];
        let observed = run(p);
        assert_durable_failure_stops_engine(p, &observed, 1);
        assert!(observed.events_of("tool_results_projected").is_empty());
        assert!(matches!(
            &observed.result,
            Err(PortError::HostFailure { message, .. })
                if message.equals_str("Controlled durable callback rejection")
        ));
    }
    /// Profile 8 (Original: onDurableMessage threw on the projected pair;
    /// models 1, durables 2, captures 0, run threw the same plain Error).
    #[test]
    fn profile_8_durable_pair_rejection_stops_before_json_retry() {
        let p = &PROFILES[7];
        let observed = run(p);
        assert_durable_failure_stops_engine(p, &observed, 2);
        assert!(matches!(
            &observed.result,
            Err(PortError::HostFailure { message, .. })
                if message.equals_str("Controlled durable callback rejection")
        ));
    }
    /// Profile 9 (Original: pair callback threw code RESULT_UNKNOWN; models 1,
    /// durables 2, captures 0, run threw with code RESULT_UNKNOWN preserved).
    #[test]
    fn profile_9_durable_pair_unknown_preserves_accepted_unknown() {
        let p = &PROFILES[8];
        let observed = run(p);
        assert_durable_failure_stops_engine(p, &observed, 2);
        let expected = durable_unknown_identity(p);
        assert!(
            matches!(
                &observed.result,
                Err(PortError::AcceptedUnknown { identity, last_applied_sequence: Some(seq) })
                    if **identity == expected && *seq == 2.0
            ),
            "{}",
            observed.terminal()
        );
    }
    /// Profile 10 (Original: host tool port RESULT_UNKNOWN; models 1,
    /// toolDispatches 1, hostModuleCalls 1, durables 1, captures 0, run threw
    /// code RESULT_UNKNOWN). Rust: ExecuteBatch -> AcceptedUnknown, fail-closed.
    #[test]
    fn profile_10_host_tool_result_unknown_fails_closed_after_one_dispatch() {
        let p = &PROFILES[9];
        let observed = run(p);
        assert_common(p, &observed);
        assert_eq!((observed.pulls, observed.dispatches), (1, 1));
        assert_eq!(observed.durable_equivalent_emissions(), 1);
        assert!(observed.captures.is_empty());
        assert_eq!(observed.declines, 0);
        assert!(observed.events_of("tool_results_projected").is_empty());
        assert!(observed.events_of("tool_result").is_empty());
        let expected = host_unknown_identity(p);
        assert!(
            matches!(
                &observed.result,
                Err(PortError::AcceptedUnknown { identity, .. }) if **identity == expected
            ),
            "{}",
            observed.terminal()
        );
    }

    // ----- Negative controls (each names the mutation it detects) -----

    /// Mutation detected: dispatching the assembled calls of an errored
    /// response (e.g. dropping the projection branch so the loop falls through
    /// to ExecuteBatch). The host counts every ExecuteBatch and would also
    /// return a HostFailure the terminal assertions reject.
    #[test]
    fn control_no_tool_dispatch_after_model_error_for_profiles_1_to_9() {
        for p in &PROFILES[..9] {
            let observed = run(p);
            assert_eq!(observed.dispatches, 0, "{}", p.id);
            assert_eq!(observed.count("UNEXPECTED_TOOL_DISPATCH"), 0, "{}", p.id);
            assert!(
                observed.events_of("tool_calls_detected").is_empty(),
                "{}",
                p.id
            );
        }
    }
    /// Mutation detected: MAX_JSON_SELF_CORRECT_RETRIES != 3. With 4 the
    /// fourth error of profile 4 would be retried (5 pulls, success); with 2
    /// profile 3 would terminate in error after 3 pulls.
    #[test]
    fn control_json_self_correct_limit_is_exactly_three() {
        let four = run(&PROFILES[3]);
        assert_eq!(four.pulls, 4);
        assert_eq!(four.count("turn_continued:model_error"), 3);
        assert_eq!(
            four.result.as_ref().unwrap().result.kind,
            TurnResultKind::Error
        );
        let three = run(&PROFILES[2]);
        assert_eq!(three.pulls, 4);
        assert_eq!(three.count("turn_continued:model_error"), 3);
        assert_eq!(
            three.result.as_ref().unwrap().result.kind,
            TurnResultKind::Success
        );
    }
    /// Mutation detected: retrying without `jsonSelfCorrect` or on a code other
    /// than `invalid_tool_arguments` (loose code matching / ignored config).
    #[test]
    fn control_no_retry_when_disabled_or_other_code() {
        for p in [&PROFILES[4], &PROFILES[5]] {
            let observed = run(p);
            assert_eq!(observed.pulls, 1, "{}", p.id);
            assert_eq!(observed.count("turn_continued:model_error"), 0, "{}", p.id);
            assert!(observed.prepares.len() == 1, "{}", p.id);
            assert_eq!(
                observed.result.as_ref().unwrap().result.kind,
                TurnResultKind::Error
            );
        }
    }
    /// Mutation detected: swallowing or retrying a failed assistant/projection
    /// emission (e.g. `let _ = emit_event(..)`, or continuing the loop after
    /// the failure). Any further pull, dispatch, capture, event or a changed
    /// error class fails.
    #[test]
    fn control_durable_failure_is_not_swallowed_or_retried() {
        for p in &PROFILES[6..9] {
            let observed = run(p);
            assert_eq!(observed.pulls, 1, "{}", p.id);
            assert_eq!(observed.dispatches, 0, "{}", p.id);
            assert!(observed.captures.is_empty(), "{}", p.id);
            assert!(observed.result.is_err(), "{}", p.id);
            let rejected_index = observed
                .log
                .iter()
                .position(|entry| entry.ends_with(":REJECTED"))
                .unwrap_or_else(|| panic!("{}: rejection not observed", p.id));
            assert_eq!(
                rejected_index + 1,
                observed.log.len(),
                "{}: {:?}",
                p.id,
                observed.log
            );
        }
    }
    /// Mutation detected: downgrading AcceptedUnknown to an ordinary failure
    /// (e.g. mapping it to HostFailure/InvalidResponse, or treating the
    /// ExecuteBatch failure as a rejected batch with synthetic results) or
    /// replaying the accepted effect (a second pull/dispatch).
    #[test]
    fn control_accepted_unknown_is_not_downgraded_or_replayed() {
        let durable = run(&PROFILES[8]);
        assert!(
            matches!(&durable.result, Err(PortError::AcceptedUnknown { .. })),
            "{}",
            durable.terminal()
        );
        assert_eq!((durable.pulls, durable.dispatches), (1, 0));
        let host = run(&PROFILES[9]);
        assert!(
            matches!(&host.result, Err(PortError::AcceptedUnknown { .. })),
            "{}",
            host.terminal()
        );
        assert_eq!((host.pulls, host.dispatches), (1, 1));
        assert!(host.events_of("tool_results_projected").is_empty());
        assert!(host.events_of("turn_completed").is_empty());
        assert!(host.captures.is_empty());
    }
}
#[cfg(test)]
mod pre_model_abort_tests {
    use super::*;
    use crate::ports::{Cancellation, ExecutionIdentity};
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<&'static str>>>;
    struct Clock(Log);
    impl TurnClock for Clock {
        fn now_iso(&self) -> JsString {
            self.0.lock().unwrap().push("result_clock");
            "2026-01-01T00:00:00.000Z".into()
        }
        fn uuid(&self) -> JsString {
            "id".into()
        }
    }
    struct Preparation {
        log: Log,
        error: Option<PortError>,
    }
    impl TurnPreparationPort for Preparation {
        fn finish_pending_effects(&mut self) -> PortResult<()> {
            self.log.lock().unwrap().push("pending_fence");
            self.error.take().map_or(Ok(()), Err)
        }
        fn prepare(
            &mut self,
            _: &mut dyn HostPort,
            _: &CallControl,
            _: &TurnRuntimeConfig,
            _: &TurnInput,
            _: &[ProjectedMessage],
        ) -> PortResult<(CapturedRequest, ModelHints)> {
            panic!("no preparation after terminal cancellation")
        }
        fn project_assembly(&self, _: &BoundarySnapshot) -> PortResult<TurnAssembly> {
            panic!("no stream projection before model invocation")
        }
    }
    struct Host {
        log: Log,
        error: Option<PortError>,
    }
    impl HostPort for Host {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            module == "context" && operation == "capture_turn"
        }
        fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
            assert!(
                control.check().is_ok(),
                "capture uses bounded cleanup, not cancelled execution control"
            );
            assert!(control.deadline.is_some());
            let HostCall::CaptureTurn(capture) = call else {
                panic!("no new model/tool effect")
            };
            assert!(!capture.errored);
            assert_eq!(capture.messages.len(), 2);
            self.log.lock().unwrap().push("capture_false");
            self.error.take().map_or(Ok(HostReply::CapturedTurn), Err)
        }
    }
    struct Events(Log);
    impl TurnEventPort for Events {
        fn emit(&mut self, _: BoundarySnapshot) -> PortResult<()> {
            panic!("cancelled execution must use accepted delivery")
        }
        fn emit_accepted(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            let kind = event
                .value()
                .get("type")
                .and_then(JsValue::as_string)
                .unwrap();
            let label = if kind.equals_str("agent_status") {
                "status"
            } else if kind.equals_str("turn_completed") {
                "completed"
            } else {
                panic!("unexpected terminal event")
            };
            self.0.lock().unwrap().push(label);
            Ok(())
        }
    }
    fn message(text: &str) -> ProjectedMessage {
        let mut value = JsObject::new();
        value.insert("role".into(), "user".into());
        value.insert("id".into(), text.into());
        ProjectedMessage {
            boundary: BoundarySnapshot::capture(&JsValue::Object(value)),
            normalized: MessageProjection::BoundaryOnly(vec![
                crate::ProjectionGap::OtherBoundaryShape,
            ]),
        }
    }
    fn input() -> TurnInput {
        TurnInput {
            session_id: "session".into(),
            turn_id: "turn".into(),
            messages: vec![],
            max_turns: None,
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        }
    }
    fn unknown() -> PortError {
        PortError::AcceptedUnknown {
            identity: Box::new(ExecutionIdentity {
                session_id: "session".into(),
                turn_id: "turn".into(),
                run_id: "run".into(),
                operation_id: "operation".into(),
                request_id: "accepted-request".into(),
                idempotency_key: None,
                module_instance_id: "module".into(),
                connection_generation: "generation".into(),
            }),
            last_applied_sequence: Some(9.0),
        }
    }
    #[derive(Clone, Copy)]
    enum AbortPath {
        PreModel,
        AfterTools,
        Stream,
    }
    fn run(
        error: PortError,
        pending: Option<PortError>,
        capture: Option<PortError>,
    ) -> (PortResult<TurnOutput>, Vec<&'static str>) {
        run_path(AbortPath::PreModel, error, pending, capture)
    }
    fn run_path(
        path: AbortPath,
        error: PortError,
        pending: Option<PortError>,
        capture: Option<PortError>,
    ) -> (PortResult<TurnOutput>, Vec<&'static str>) {
        let log = Arc::new(Mutex::new(vec![]));
        let state = TurnExecutionState {
            messages: vec![message("prior"), message("unconsumed-prompt")],
            turn_count: 1.0,
            usage: empty_usage(),
            permission_denials: vec![],
            final_message: Some(message("prior-final")),
            structured_output: None,
            budget: BudgetLedger::new(None, None, 0.0),
            timeline: TurnTimeline::new("turn".into()),
            transient_prompt_ids: vec!["unconsumed-prompt".into()],
        };
        let mut host = Host {
            log: log.clone(),
            error: capture,
        };
        let mut preparation = Preparation {
            log: log.clone(),
            error: pending,
        };
        let mut events = Events(log.clone());
        let clock = Clock(log.clone());
        let cleanup = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let input = input();
        let result = match path {
            AbortPath::PreModel => finish_pre_model_abort(
                state,
                "started".into(),
                error,
                &mut host,
                &mut preparation,
                &mut events,
                &clock,
                &input,
                &cleanup,
            ),
            AbortPath::AfterTools | AbortPath::Stream => {
                let PortError::Cancelled { cause } = error else {
                    panic!("abort path requires a cancellation")
                };
                if matches!(path, AbortPath::AfterTools) {
                    finish_aborted_after_tools(
                        state,
                        "started".into(),
                        &clock,
                        TerminalTurnPorts {
                            host: &mut host,
                            events: &mut events,
                            input: &input,
                            control: &cleanup,
                        },
                        cause == CancelCause::Deadline,
                    )
                } else {
                    finish_aborted_stream(
                        state,
                        "started".into(),
                        BoundarySnapshot::capture(
                            &pilotdeck_protocol::parse_json(r#"{"toolCalls":[{}]}"#).unwrap(),
                        ),
                        cause,
                        AbortedStreamBranch::StreamThrew,
                        AbortedTurnPorts {
                            host: &mut host,
                            preparation: &preparation,
                            events: &mut events,
                            clock: &clock,
                            input: &input,
                            control: &cleanup,
                        },
                    )
                }
            }
        };
        let observed = log.lock().unwrap().clone();
        (result, observed)
    }
    fn assert_capture_cancellation_policy(path: AbortPath) {
        for deadline in [false, true] {
            for cause in [
                CancelCause::Deadline,
                CancelCause::Requested("local cancel".into()),
                CancelCause::OwnerRetired,
                CancelCause::TransportClosed,
            ] {
                let hard_stop = matches!(
                    cause,
                    CancelCause::OwnerRetired | CancelCause::TransportClosed
                );
                let (result, log) = run_path(
                    path,
                    PortError::Cancelled {
                        cause: if deadline {
                            CancelCause::Deadline
                        } else {
                            CancelCause::Requested("user cancel".into())
                        },
                    },
                    None,
                    Some(PortError::Cancelled {
                        cause: cause.clone(),
                    }),
                );
                assert!(log.contains(&"capture_false"));
                if deadline && !hard_stop {
                    let output = result.unwrap();
                    assert_eq!(output.result.kind, TurnResultKind::Aborted);
                    assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
                    assert_eq!(output.messages.len(), 2);
                    assert_eq!(log.last(), Some(&"completed"));
                } else {
                    assert!(
                        matches!(result, Err(PortError::Cancelled { cause: actual }) if actual == cause)
                    );
                    assert!(!log.contains(&"completed"));
                }
            }
        }
    }
    #[test]
    fn pre_model_capture_local_cancel_completes_only_for_execution_deadline() {
        assert_capture_cancellation_policy(AbortPath::PreModel);
    }
    #[test]
    fn after_tools_capture_local_cancel_completes_only_for_execution_deadline() {
        assert_capture_cancellation_policy(AbortPath::AfterTools);
    }
    #[test]
    fn stream_capture_local_cancel_completes_only_for_execution_deadline() {
        assert_capture_cancellation_policy(AbortPath::Stream);
    }
    #[test]
    fn known_pre_model_cancel_retains_messages_and_creates_result_before_status_capture() {
        let (result, log) = run(
            PortError::Cancelled {
                cause: CancelCause::Requested("Cancel requested".into()),
            },
            None,
            None,
        );
        let output = result.unwrap();
        assert_eq!(output.result.kind, TurnResultKind::Aborted);
        assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
        assert_eq!(output.result.turns, 1.0);
        assert_eq!(output.messages.len(), 2);
        assert!(output.result.final_message.is_some());
        assert_eq!(
            log,
            vec![
                "pending_fence",
                "result_clock",
                "status",
                "capture_false",
                "completed"
            ]
        );
    }
    #[test]
    fn quiet_source_abort_reason_omits_status() {
        let (result, log) = run(
            PortError::Cancelled {
                cause: CancelCause::Requested("plain reason".into()),
            },
            None,
            None,
        );
        assert!(result.is_ok());
        assert_eq!(
            log,
            vec![
                "pending_fence",
                "result_clock",
                "capture_false",
                "completed"
            ]
        );
    }
    #[test]
    fn pending_unknown_prevents_capture_and_known_completion() {
        let (result, log) = run(
            PortError::Cancelled {
                cause: CancelCause::Requested("cancel".into()),
            },
            Some(unknown()),
            None,
        );
        assert!(matches!(
            result,
            Err(PortError::AcceptedUnknown {
                last_applied_sequence: Some(9.0),
                ..
            })
        ));
        assert_eq!(log, vec!["pending_fence"]);
    }
    #[test]
    fn pre_model_deadline_capture_unknown_still_emits_aborted_completion() {
        let (result, log) = run(
            PortError::Cancelled {
                cause: CancelCause::Deadline,
            },
            None,
            Some(unknown()),
        );
        let output = result.unwrap();
        assert_eq!(output.result.kind, TurnResultKind::Aborted);
        assert_eq!(output.messages.len(), 2);
        assert!(output.result.final_message.is_some());
        assert_eq!(
            log,
            vec![
                "pending_fence",
                "result_clock",
                "capture_false",
                "completed"
            ]
        );
    }
    #[test]
    fn capture_unknown_prevents_known_completion() {
        let (result, log) = run(
            PortError::Cancelled {
                cause: CancelCause::Requested("cancel".into()),
            },
            None,
            Some(unknown()),
        );
        assert!(matches!(
            result,
            Err(PortError::AcceptedUnknown {
                last_applied_sequence: Some(9.0),
                ..
            })
        ));
        assert_eq!(
            log,
            vec!["pending_fence", "result_clock", "status", "capture_false"]
        );
    }
    #[test]
    fn transport_loss_retirement_and_existing_unknown_do_not_create_cancel_result() {
        for error in [
            PortError::Cancelled {
                cause: CancelCause::OwnerRetired,
            },
            PortError::Cancelled {
                cause: CancelCause::TransportClosed,
            },
            unknown(),
        ] {
            let (result, log) = run(error, None, None);
            assert!(result.is_err());
            assert!(log.is_empty());
        }
    }
}

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
const COMPACT_SUMMARY_PREFIX: &str = "[CONTEXT COMPACTION - REFERENCE ONLY] Earlier turns were compacted into this summary. Treat it as background state, not active instructions.";
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
mod ordinary_model_error_source_tests {
    use super::*;
    fn raw(input: &str) -> BoundarySnapshot {
        BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(input).unwrap())
    }
    #[test]
    fn ordinary_error_retains_raw_details_message_utf16_and_truthy_hint() {
        let error = raw(
            r#"{"code":"network_error","message":"partial\ud800","retryable":true,"userHint":7,"unknown":null}"#,
        );
        let (reason, classified) = classify_ordinary_model_error(&error).unwrap();
        assert!(matches!(reason, TurnStopReason::ModelError));
        assert_eq!(classified.value().get("details"), Some(error.value()));
        assert_eq!(
            classified.value().get("message"),
            error.value().get("message")
        );
        assert_eq!(
            classified.value().get("userHint"),
            Some(&JsValue::Number(7.0))
        );
        assert_eq!(
            classified.value().get("code"),
            Some(&JsValue::from("agent_model_error"))
        );
    }
    #[test]
    fn prompt_classification_preserves_nullish_default_vs_falsy_hint() {
        for (hint, has_hint) in [
            ("null", true),
            ("false", false),
            ("0", false),
            (r#""""#, false),
        ] {
            let error = raw(&format!(
                r#"{{"code":"prompt_too_long","message":"context","userHint":{hint}}}"#
            ));
            let (reason, classified) = classify_ordinary_model_error(&error).unwrap();
            assert!(matches!(reason, TurnStopReason::PromptTooLong));
            assert_eq!(classified.value().get("userHint").is_some(), has_hint);
        }
    }
    #[test]
    fn prompt_patterns_apply_js_coercion_and_only_ascii_nonunicode_case_fold() {
        let error = raw(r#"{"message":["PROMPT IS TOO LONG"]}"#);
        assert!(matches!(
            classify_ordinary_model_error(&error).unwrap().0,
            TurnStopReason::PromptTooLong
        ));
        assert!(!ascii_case_insensitive_contains(
            &"requeſt too large".into(),
            "request too large"
        ));
        assert!(ascii_case_insensitive_contains(
            &"REQUEST TOO LARGE\n".into(),
            "request too large"
        ));
        assert_eq!(
            model_error_regexp_text(raw(r#"[null,["a",null],false]"#).value()).unwrap(),
            JsString::from(",a,,false")
        );
    }
    #[test]
    fn reasoning_retry_gate_uses_exact_words_case_and_source_whitespace() {
        for (message, expected) in [
            ("reasoning_content THINKING\u{a0}MODE passed back", true),
            ("xreasoning_content thinking mode passed back", false),
            ("reasoning_content thinking mode pass back", true),
            ("reasoning_content thinkingmode passed back", false),
        ] {
            let mut raw = JsObject::new();
            raw.insert("message".into(), message.into());
            assert_eq!(
                requires_reasoning_content_retry(&BoundarySnapshot::capture(&JsValue::Object(raw)))
                    .unwrap(),
                expected
            );
        }
    }
    #[test]
    fn regex_coercion_exception_occurs_only_after_source_shortcircuits() {
        let error = raw(r#"{"message":{"toString":null}}"#);
        assert!(
            matches!(classify_ordinary_model_error(&error), Err(PortError::SourceException { message, .. }) if message.equals_str("Cannot convert object to primitive value"))
        );
        let error = raw(r#"{"code":"prompt_too_long","message":{"toString":null}}"#);
        assert!(classify_ordinary_model_error(&error).is_ok());
        let error = raw(r#"{"recoverableViaCompact":true,"message":{"toString":null}}"#);
        assert!(classify_ordinary_model_error(&error).is_ok());
    }
}

#[cfg(test)]
mod ordinary_model_error_terminal_order_tests {
    use super::*;
    use crate::ports::{Cancellation, Captured, ExecutionIdentity, LifecycleResult};
    use std::sync::{Arc, Mutex};
    type Log = Arc<Mutex<Vec<JsString>>>;
    struct Clock(Log);
    impl TurnClock for Clock {
        fn now_iso(&self) -> JsString {
            self.0.lock().unwrap().push("result_clock".into());
            "end".into()
        }
        fn uuid(&self) -> JsString {
            "id".into()
        }
    }
    struct Events(Log);
    impl TurnEventPort for Events {
        fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            self.0.lock().unwrap().push(
                event
                    .value()
                    .get("type")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .clone(),
            );
            Ok(())
        }
    }
    struct Host {
        log: Log,
        model_error: BoundarySnapshot,
        capture_error: Option<PortError>,
    }
    impl HostPort for Host {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            matches!(
                (module, operation),
                ("lifecycle", "dispatch") | ("context", "capture_turn")
            )
        }
        fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
            match call {
                HostCall::DispatchLifecycle { event, payload } => {
                    assert!(event.equals_str("StopFailure"));
                    assert_eq!(
                        payload.unwrap().get("error"),
                        Some(self.model_error.value())
                    );
                    self.log.lock().unwrap().push("StopFailure".into());
                    Ok(HostReply::Lifecycle(Captured {
                        view: LifecycleResult {
                            effects: vec![],
                            messages: vec![],
                            events: vec![],
                            blocking_errors: vec![],
                            non_blocking_errors: vec![],
                            pending_async_hooks: None,
                        },
                        boundary: BoundarySnapshot::capture(&JsValue::Null),
                    }))
                }
                HostCall::CaptureTurn(capture) => {
                    assert!(capture.errored);
                    self.log.lock().unwrap().push("capture_true".into());
                    self.capture_error
                        .take()
                        .map_or(Ok(HostReply::CapturedTurn), Err)
                }
                _ => panic!("no model/tool replay in terminal phase"),
            }
        }
    }
    fn run(
        late_exception: bool,
        capture_error: Option<PortError>,
    ) -> (PortResult<TurnOutput>, Vec<JsString>) {
        let log: Log = Arc::new(Mutex::new(vec![]));
        let raw = BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(r#"{"code":"network_error","message":"partial\ud800","provider":"parity","protocol":"openai","retryable":false}"#).unwrap());
        let (reason, classified) = classify_ordinary_model_error(&raw).unwrap();
        let mut state = TurnExecutionState {
            messages: vec![],
            turn_count: 2.0,
            usage: empty_usage(),
            permission_denials: vec![],
            final_message: None,
            structured_output: Some(BoundarySnapshot::capture(&JsValue::from(
                "earlier-tool-data",
            ))),
            budget: BudgetLedger::new(None, None, 0.0),
            timeline: TurnTimeline::new("turn".into()),
            transient_prompt_ids: vec![],
        };
        let input = TurnInput {
            session_id: "session".into(),
            turn_id: "turn".into(),
            messages: vec![],
            max_turns: None,
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let mut host = Host {
            log: log.clone(),
            model_error: raw.clone(),
            capture_error,
        };
        let mut events = Events(log.clone());
        let clock = Clock(log.clone());
        let result = finish_ordinary_model_error(
            &mut state,
            "start".into(),
            &raw,
            classified,
            reason,
            |_, error| {
                log.lock().unwrap().push("build_status".into());
                if late_exception {
                    return Err(PortError::SourceException {
                        message: "providerMessage.trim is not a function".into(),
                        boundary: error.clone(),
                    });
                }
                Ok(BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(r#"{"event":"model_request_failed","text":"provider failure\n\nAction: check network","detail":{"visible":true}}"#).unwrap()))
            },
            &clock,
            TerminalTurnPorts {
                host: &mut host,
                events: &mut events,
                input: &input,
                control: &control,
            },
        );
        assert!(
            state.structured_output.is_some(),
            "result omission must not erase turn state"
        );
        let recorded = log.lock().unwrap().clone();
        (result, recorded)
    }
    #[test]
    fn budget_status_projection_preserves_failure_diagnostics_for_both_limits() {
        struct BudgetEvents(Vec<BoundarySnapshot>);
        impl TurnEventPort for BudgetEvents {
            fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
                self.0.push(event);
                Ok(())
            }
        }
        for (stop, event_name, reason) in [
            (
                crate::driver::BudgetStop::Turn,
                "max_budget_reached",
                TurnStopReason::MaxBudget,
            ),
            (
                crate::driver::BudgetStop::Task,
                "task_budget_reached",
                TurnStopReason::TaskBudget,
            ),
        ] {
            let log: Log = Arc::new(Mutex::new(vec![]));
            let mut ledger = BudgetLedger::new(
                (stop == crate::driver::BudgetStop::Turn).then_some(0.1),
                (stop == crate::driver::BudgetStop::Task).then_some(0.1),
                0.0,
            );
            ledger.turn_spent_usd = 0.125;
            ledger.task_spent_usd = 0.125;
            let assistant = ProjectedMessage {
                boundary: BoundarySnapshot::capture(
                    &pilotdeck_protocol::parse_json(r#"{"role":"assistant","content":[]}"#)
                        .unwrap(),
                ),
                normalized: MessageProjection::BoundaryOnly(vec![]),
            };
            let route = crate::ports::Route {
                provider: "parity".into(),
                model: "model".into(),
            };
            let plan = crate::budget_terminal::OriginalBudgetTerminalPolicy
                .evaluate(crate::budget_terminal::BudgetTerminalInput {
                    ledger: &ledger,
                    stop,
                    last_invocation_cost_usd: Some(0.125),
                    route: &route,
                    assistant_message: &assistant,
                    unexecuted_tool_call_count: 1,
                })
                .unwrap();
            let error = plan.error.clone();
            let diagnostic = error.value().get("message").unwrap().clone();
            assert!(
                diagnostic
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap()
                    .starts_with("Reached Gateway-owned")
            );
            let detail = plan.status_detail.clone();
            let state = TurnExecutionState {
                messages: vec![],
                turn_count: 1.0,
                usage: empty_usage(),
                permission_denials: vec![],
                final_message: None,
                structured_output: None,
                budget: ledger,
                timeline: TurnTimeline::new("turn".into()),
                transient_prompt_ids: vec![],
            };
            let input = TurnInput {
                session_id: "session".into(),
                turn_id: "turn".into(),
                messages: vec![],
                max_turns: None,
                max_budget_usd: None,
                task_budget_usd: None,
                initial_task_budget_spent_usd: None,
                base_permission_mode: None,
                allow_plan_mode_tools: false,
                can_elicit: false,
                model_override: None,
            };
            let control = CallControl {
                cancellation: Cancellation::default(),
                deadline: None,
            };
            let mut host = Host {
                log: log.clone(),
                model_error: BoundarySnapshot::capture(&diagnostic),
                capture_error: None,
            };
            let mut events = BudgetEvents(vec![]);
            let output = finish_budget_terminal(
                state,
                "start".into(),
                plan,
                &Clock(log.clone()),
                TerminalTurnPorts {
                    host: &mut host,
                    events: &mut events,
                    input: &input,
                    control: &control,
                },
            )
            .unwrap();
            assert_eq!(output.result.kind, TurnResultKind::Error);
            assert_eq!(output.result.stop_reason, reason);
            assert_eq!(output.result.errors.len(), 1);
            assert_eq!(output.result.errors[0].value(), error.value());
            let values: Vec<_> = events.0.iter().map(BoundarySnapshot::value).collect();
            assert_eq!(
                values
                    .iter()
                    .map(|v| v.get("type").unwrap().clone())
                    .collect::<Vec<_>>(),
                [
                    "stop_failure",
                    "agent_status",
                    "turn_failed",
                    "turn_completed"
                ]
                .map(JsValue::from)
            );
            assert_eq!(values[0].get("error"), Some(&diagnostic));
            assert_eq!(values[1].get("event"), Some(&JsValue::from(event_name)));
            assert_eq!(values[1].get("detail"), Some(detail.value()));
            // TS sidecarStatusMessage falls back to event.event when these
            // fields are absent. Numeric budget detail must remain intact.
            assert!(values[1].get("text").is_none());
            assert!(detail.value().get("message").is_none());
            assert_eq!(values[2].get("error"), Some(error.value()));
            assert_eq!(
                *log.lock().unwrap(),
                ["StopFailure", "result_clock", "capture_true"].map(JsString::from)
            );
        }
    }
    #[test]
    fn ordinary_terminal_has_source_order_and_omits_prior_structured_output() {
        let (output, log) = run(false, None);
        let output = output.unwrap();
        assert!(matches!(output.result.kind, TurnResultKind::Error));
        assert!(output.result.structured_output.is_none());
        assert_eq!(output.result.errors.len(), 1);
        assert_eq!(
            log,
            [
                "StopFailure",
                "stop_failure",
                "result_clock",
                "build_status",
                "agent_status",
                "turn_failed",
                "capture_true",
                "turn_completed"
            ]
            .map(JsString::from)
        );
    }
    #[test]
    fn late_status_exception_preserves_prior_phases_and_prevents_capture_completion() {
        let (result, log) = run(true, None);
        assert!(matches!(result, Err(PortError::SourceException { .. })));
        assert_eq!(
            log,
            [
                "StopFailure",
                "stop_failure",
                "result_clock",
                "build_status"
            ]
            .map(JsString::from)
        );
    }
    #[test]
    fn accepted_capture_unknown_retains_identity_and_never_publishes_completion() {
        let identity = ExecutionIdentity {
            session_id: "session".into(),
            turn_id: "turn".into(),
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "accepted-capture".into(),
            idempotency_key: None,
            module_instance_id: "module".into(),
            connection_generation: "generation".into(),
        };
        let (result, log) = run(
            false,
            Some(PortError::AcceptedUnknown {
                identity: Box::new(identity.clone()),
                last_applied_sequence: Some(19.0),
            }),
        );
        assert!(
            matches!(result,Err(PortError::AcceptedUnknown { identity:actual,last_applied_sequence:Some(19.0) }) if *actual==identity)
        );
        assert_eq!(log.last(), Some(&JsString::from("capture_true")));
        assert!(!log.iter().any(|kind| kind.equals_str("turn_completed")));
    }
}

#[cfg(test)]
mod ordinary_model_error_recovery_tests {
    use super::*;
    use crate::ports::{Cancellation, ExecutionIdentity};
    struct Host {
        reply: Option<PortResult<HostReply>>,
        advertised: bool,
        calls: usize,
        error: BoundarySnapshot,
    }
    impl HostPort for Host {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            self.advertised && module == "context" && operation == "recover_from_model_error"
        }
        fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
            let HostCall::RecoverModelErrorBoundary(recovery) = call else {
                panic!("only one source recovery probe")
            };
            assert_eq!(recovery.error.value(), self.error.value());
            assert!(!recovery.has_attempted_compact);
            self.calls += 1;
            self.reply.take().expect("never replay accepted probe")
        }
    }
    fn run(
        reply: PortResult<HostReply>,
        advertised: bool,
    ) -> (PortResult<Option<crate::ports::RecoveryDecision>>, usize) {
        let error = BoundarySnapshot::capture(
            &pilotdeck_protocol::parse_json(
                r#"{"code":"network_error","message":"failure","future":null}"#,
            )
            .unwrap(),
        );
        let mut host = Host {
            reply: Some(reply),
            advertised,
            calls: 0,
            error: error.clone(),
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let input = TurnInput {
            session_id: "session".into(),
            turn_id: "turn".into(),
            messages: vec![],
            max_turns: None,
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        };
        let result =
            probe_ordinary_model_error_recovery(&mut host, &control, &input, &[], &error, false);
        (result, host.calls)
    }
    #[test]
    fn source_probe_giveup_falsy_future_and_known_exception_do_not_retry() {
        for json in [
            r#"{"type":"give_up"}"#,
            r#"{"type":"future"}"#,
            "null",
            "false",
        ] {
            let (result, calls) = run(
                Ok(HostReply::RecoveryBoundaryOnly(BoundarySnapshot::capture(
                    &pilotdeck_protocol::parse_json(json).unwrap(),
                ))),
                true,
            );
            assert!(matches!(result, Ok(None)));
            assert_eq!(calls, 1);
        }
        let (result, calls) = run(
            Err(PortError::HostFailure {
                code: "known".into(),
                message: "failure".into(),
                boundary: BoundarySnapshot::capture(&JsValue::Null),
            }),
            true,
        );
        assert!(matches!(result, Ok(None)));
        assert_eq!(calls, 1);
        let (result, calls) = run(
            Err(PortError::HostFailure {
                code: "not_called".into(),
                message: "not called".into(),
                boundary: BoundarySnapshot::capture(&JsValue::Null),
            }),
            false,
        );
        assert!(matches!(result, Ok(None)));
        assert_eq!(calls, 0);
    }
    #[test]
    fn accepted_unknown_probe_identity_remains_terminal_and_special_action_is_not_faked_giveup() {
        let identity = ExecutionIdentity {
            session_id: "session".into(),
            turn_id: "turn".into(),
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "accepted-recovery".into(),
            idempotency_key: None,
            module_instance_id: "module".into(),
            connection_generation: "generation".into(),
        };
        let (result, calls) = run(
            Err(PortError::AcceptedUnknown {
                identity: Box::new(identity.clone()),
                last_applied_sequence: Some(19.0),
            }),
            true,
        );
        assert!(
            matches!(result,Err(PortError::AcceptedUnknown {identity:actual,last_applied_sequence:Some(19.0)}) if *actual==identity)
        );
        assert_eq!(calls, 1);
        let (result, calls) = run(
            Ok(HostReply::RecoveryBoundaryOnly(BoundarySnapshot::capture(
                &pilotdeck_protocol::parse_json(
                    r#"{"type":"truncate_head_and_retry","keepRatio":0.5}"#,
                )
                .unwrap(),
            ))),
            true,
        );
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}

#[cfg(test)]
mod owning_reactive_recovery_tests {
    //! OWNING-path tests for Original AgentLoop.ts:1386-1696 reactive
    //! model-error recovery (a5 contract profiles P01-P03, P08-P12 and the
    //! negative controls), driving `run_admitted_turn` through in-memory ports
    //! with scripted provider replies and scripted host decisions. The pure
    //! helper fixtures are the independent verifier's hand-derived tables
    //! (evidence/CORE01-reactive-source-expectations-independent-a6 §B, OPEN-5).
    use super::*;
    use crate::ports::{
        BudgetStage, Cancellation, CompactionCommit, CompactionIntent, CompactionTrigger,
        ExecutionHints, OutputLimitScope, RecoveryDecision, RequestProjection, Route,
        RuntimeContextSurface, StreamChunk, StreamEvent,
    };
    use crate::{ToolContent, ToolExecutionResult, ToolResultDisposition};
    use pilotdeck_protocol::model::{PreparedWireRequest, ProviderEcho, ToolCall};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::Arc;

    const NOW: &str = "2026-10-11T00:00:00.000Z";
    const MAX_OUTPUT_PROMPT: &str = "Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.";
    const REASONING_MESSAGE: &str =
        "reasoning_content is required in thinking mode and must be passed back";

    fn json(text: &str) -> JsValue {
        pilotdeck_protocol::parse_json(text).unwrap()
    }
    fn str_of(value: &JsValue, key: &str) -> Option<String> {
        value
            .get(key)
            .and_then(JsValue::as_string)
            .map(|s| s.to_utf8().unwrap())
    }
    fn num_of(value: &JsValue, key: &str) -> Option<f64> {
        value.get(key).and_then(JsValue::as_number)
    }
    fn message(text: &str) -> ProjectedMessage {
        ProjectedMessage {
            boundary: BoundarySnapshot::capture(&json(text)),
            normalized: MessageProjection::BoundaryOnly(vec![
                crate::ProjectionGap::OtherBoundaryShape,
            ]),
        }
    }
    fn user(text: &str) -> ProjectedMessage {
        message(&format!(
            r#"{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}"#
        ))
    }
    fn assistant(text: &str) -> ProjectedMessage {
        message(&format!(
            r#"{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}"#
        ))
    }
    fn blocks(role: &str, blocks: &str) -> ProjectedMessage {
        message(&format!(r#"{{"role":"{role}","content":[{blocks}]}}"#))
    }
    fn tc(id: &str) -> String {
        format!(r#"{{"type":"tool_call","id":"{id}","name":"t","input":{{}}}}"#)
    }
    fn tr(id: &str) -> String {
        format!(
            r#"{{"type":"tool_result","toolCallId":"{id}","content":[{{"type":"text","text":"r"}}]}}"#
        )
    }
    fn text(text: &str) -> String {
        format!(r#"{{"type":"text","text":"{text}"}}"#)
    }
    /// Compact observable signature: role, block kinds with ids/texts, and
    /// whether a `metadata` key exists (the continuation sentinel has none).
    fn block_sig(block: &JsValue) -> String {
        let kind = str_of(block, "type").unwrap();
        match kind.as_str() {
            "text" => format!("text({})", str_of(block, "text").unwrap()),
            "thinking" => format!(
                "thinking({}|{})",
                str_of(block, "text").unwrap_or_else(|| "-".into()),
                str_of(block, "reasoningContent").unwrap_or_else(|| "-".into())
            ),
            "tool_call" => format!("tool_call#{}", str_of(block, "id").unwrap()),
            "tool_result" | "tool_result_reference" | "media_reference" => {
                let inner = match block.get("content") {
                    Some(JsValue::Array(items)) => format!(
                        "{{{}}}",
                        items.iter().map(block_sig).collect::<Vec<_>>().join(",")
                    ),
                    _ => String::new(),
                };
                format!(
                    "{kind}#{}{inner}",
                    str_of(block, "toolCallId").unwrap_or_default()
                )
            }
            other => other.to_string(),
        }
    }
    fn sig(value: &JsValue) -> String {
        let JsValue::Array(content) = value.get("content").unwrap() else {
            panic!("content array");
        };
        format!(
            "{}:[{}]{}",
            str_of(value, "role").unwrap(),
            content.iter().map(block_sig).collect::<Vec<_>>().join(","),
            if value.get("metadata").is_some() {
                "+meta"
            } else {
                ""
            }
        )
    }
    fn sigs(messages: &[ProjectedMessage]) -> Vec<String> {
        messages.iter().map(|m| sig(m.boundary.value())).collect()
    }
    fn cont_sig() -> String {
        format!("user:[text({CONTINUATION_TEXT})]")
    }

    // ----- scripted provider replies and host decisions -----
    #[derive(Clone)]
    enum Reply {
        Error { error: String, tool_calls: usize },
        Success,
    }
    fn error_reply(error: &str, tool_calls: usize) -> Reply {
        Reply::Error {
            error: error.to_string(),
            tool_calls,
        }
    }
    #[derive(Clone)]
    enum Decision {
        Typed(RecoveryDecision),
        Raw(&'static str),
        Failure,
    }
    fn adjust(max_output_tokens: f64, scope: Option<OutputLimitScope>) -> Decision {
        Decision::Typed(RecoveryDecision::AdjustOutputAndRetry {
            max_output_tokens,
            reason: "provider-output-cap".into(),
            scope,
        })
    }
    fn truncate(keep_ratio: f64, reason: &str) -> Decision {
        Decision::Typed(RecoveryDecision::TruncateHeadAndRetry {
            keep_ratio,
            reason: reason.into(),
        })
    }
    fn give_up() -> Decision {
        Decision::Typed(RecoveryDecision::GiveUp {
            reason: "non_recoverable_model_error".into(),
        })
    }
    fn strip_images() -> Decision {
        Decision::Typed(RecoveryDecision::StripImagesAndRetry {
            reason: "image-too-large".into(),
        })
    }
    fn compact() -> Decision {
        Decision::Typed(RecoveryDecision::CompactAndRetry {
            max_context_tokens: Some(100_000.0),
            max_output_tokens: None,
            reason: "provider-context-cap".into(),
        })
    }
    fn compact_with_output(max_context_tokens: f64, max_output_tokens: f64) -> Decision {
        Decision::Typed(RecoveryDecision::CompactAndRetry {
            max_context_tokens: Some(max_context_tokens),
            max_output_tokens: Some(max_output_tokens),
            reason: "provider-available-output-too-small".into(),
        })
    }
    /// Scripted `try_auto_compact` reply: a raw compact boundary or a host
    /// failure with the given code.
    #[derive(Clone)]
    enum CompactReply {
        Boundary(&'static str),
        Failure(&'static str),
    }
    /// Scripted fixture behaviour of `prepare_recovery_compaction`.
    #[derive(Clone, Copy)]
    enum PrepReply {
        Intent,
        Absent,
        Advisory,
        Fatal,
    }
    #[derive(Clone, Copy)]
    enum PersistReply {
        Committed,
        Failure,
    }
    fn host_failure(code: &str) -> PortError {
        PortError::HostFailure {
            code: code.into(),
            message: "controlled host failure".into(),
            boundary: BoundarySnapshot::capture(&JsValue::Null),
        }
    }
    struct Script {
        replies: Vec<Reply>,
        decisions: Vec<Decision>,
        advertise_recovery: bool,
        advertise_compact: bool,
        advertise_persist: bool,
        compact_replies: Vec<CompactReply>,
        persist_reply: PersistReply,
        recovery_prep: PrepReply,
        config_cap: Option<f64>,
        catalog: Option<f64>,
        initial: Vec<ProjectedMessage>,
    }
    impl Script {
        fn new(replies: Vec<Reply>, decisions: Vec<Decision>) -> Self {
            Script {
                replies,
                decisions,
                advertise_recovery: true,
                advertise_compact: true,
                advertise_persist: true,
                compact_replies: vec![],
                persist_reply: PersistReply::Committed,
                recovery_prep: PrepReply::Intent,
                config_cap: Some(4000.0),
                catalog: Some(16000.0),
                initial: vec![user("Controlled request")],
            }
        }
    }
    /// Observable projection of one `try_auto_compact` intent.
    #[derive(Debug, PartialEq)]
    struct CompactionRecord {
        stage: BudgetStage,
        target: String,
        max_context_tokens: Option<f64>,
        reserved_output_tokens: Option<f64>,
        allow_fallback_on_failure: Option<bool>,
        messages: Vec<String>,
    }
    fn raw_event(text: &str) -> StreamEvent {
        StreamEvent::BoundaryOnly(BoundarySnapshot::capture(&json(text)))
    }
    fn stream(reply: &Reply, pull: usize) -> Vec<StreamEvent> {
        let mut events = vec![raw_event(r#"{"type":"message_start","role":"assistant"}"#)];
        match reply {
            Reply::Error { error, tool_calls } => {
                for ordinal in 0..*tool_calls {
                    let id = format!("call-{pull}-{ordinal}");
                    events.push(raw_event(&format!(
                        r#"{{"type":"tool_call_start","id":"{id}","name":"owned_tool"}}"#
                    )));
                    events.push(raw_event(&format!(
                        r#"{{"type":"tool_call_end","toolCall":{{"id":"{id}","name":"owned_tool","input":{{"ordinal":{ordinal}}},"raw":{{}}}}}}"#
                    )));
                }
                events.push(raw_event(&format!(r#"{{"type":"error","error":{error}}}"#)));
            }
            Reply::Success => {
                events.push(raw_event(
                    r#"{"type":"text_delta","text":"controlled success"}"#,
                ));
                events.push(raw_event(r#"{"type":"message_end","finishReason":"stop"}"#));
            }
        }
        events
    }
    struct Host {
        replies: Vec<Reply>,
        decisions: Vec<Decision>,
        advertise_recovery: bool,
        advertise_compact: bool,
        advertise_persist: bool,
        compact_replies: Vec<CompactReply>,
        persist_reply: PersistReply,
        pulls: usize,
        /// (hasAttemptedCompact, messages.len()) per recover_from_model_error.
        probes: Vec<(bool, usize)>,
        captures: usize,
        compactions: Vec<CompactionRecord>,
        /// (boundary, marked message sigs) per persist_compaction.
        persists: Vec<(JsValue, Vec<String>)>,
    }
    impl HostPort for Host {
        fn advertised(&self, module: &str, operation: &str) -> bool {
            match (module, operation) {
                ("model", "stream_next") | ("context", "capture_turn") => true,
                ("context", "recover_from_model_error") => self.advertise_recovery,
                ("context", "try_auto_compact") => self.advertise_compact,
                ("turn", "persist_compaction") => self.advertise_persist,
                _ => false,
            }
        }
        fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
            match call {
                HostCall::AutoCompact(intent) => {
                    self.compactions.push(CompactionRecord {
                        stage: intent.stage,
                        target: intent
                            .budget_request
                            .as_ref()
                            .map(|request| {
                                let (provider, model) = key(&request.route);
                                format!("{provider}/{model}")
                            })
                            .unwrap_or_default(),
                        max_context_tokens: intent.max_context_tokens,
                        reserved_output_tokens: intent.reserved_output_tokens,
                        allow_fallback_on_failure: intent.allow_fallback_on_failure,
                        messages: sigs(&intent.messages),
                    });
                    let reply = self
                        .compact_replies
                        .get(self.compactions.len() - 1)
                        .unwrap_or_else(|| {
                            panic!("unexpected try_auto_compact {}", self.compactions.len())
                        });
                    match reply {
                        CompactReply::Boundary(text) => Ok(HostReply::CompactedBoundaryOnly(
                            BoundarySnapshot::capture(&json(text)),
                        )),
                        CompactReply::Failure(code) => Err(host_failure(code)),
                    }
                }
                HostCall::TurnPersistCompaction { boundary, messages } => {
                    self.persists
                        .push((boundary.boundary.value().clone(), sigs(&messages)));
                    match self.persist_reply {
                        PersistReply::Committed => {
                            Ok(HostReply::CompactionCommitted(CompactionCommit {
                                response: BoundarySnapshot::capture(&JsValue::Null),
                            }))
                        }
                        PersistReply::Failure => Err(host_failure("controlled-persist")),
                    }
                }
                HostCall::ModelPull { .. } => {
                    self.pulls += 1;
                    let reply = self
                        .replies
                        .get(self.pulls - 1)
                        .unwrap_or_else(|| panic!("unexpected model pull {}", self.pulls));
                    Ok(HostReply::ModelChunk(StreamChunk {
                        events: stream(reply, self.pulls),
                        done: true,
                    }))
                }
                HostCall::RecoverModelErrorBoundary(probe) => {
                    self.probes
                        .push((probe.has_attempted_compact, probe.messages.len()));
                    let decision = self
                        .decisions
                        .get(self.probes.len() - 1)
                        .unwrap_or_else(|| panic!("unexpected probe {}", self.probes.len()));
                    match decision {
                        Decision::Typed(view) => Ok(HostReply::Recovery(crate::ports::Captured {
                            view: view.clone(),
                            boundary: BoundarySnapshot::capture(&JsValue::Null),
                        })),
                        Decision::Raw(text) => Ok(HostReply::RecoveryBoundaryOnly(
                            BoundarySnapshot::capture(&json(text)),
                        )),
                        Decision::Failure => Err(PortError::HostFailure {
                            code: "controlled".into(),
                            message: "controlled probe failure".into(),
                            boundary: BoundarySnapshot::capture(&JsValue::Null),
                        }),
                    }
                }
                HostCall::CaptureTurn(_) => {
                    self.captures += 1;
                    Ok(HostReply::CapturedTurn)
                }
                other => panic!("unexpected host call {:?}", other.method()),
            }
        }
    }
    /// Original transient cap store (A:202-209, 2587-2601, 2631-2643):
    /// current = min over {attempt ?? requested ?? session ?? config, hard,
    /// catalog(only if a base exists)}; attempt is cleared after each prepare.
    #[derive(Default, Clone)]
    struct Caps {
        requested: Option<f64>,
        session: Option<f64>,
        attempt: Option<f64>,
        hard: Option<f64>,
    }
    struct Preparation {
        caps: HashMap<(String, String), Caps>,
        /// Original transient `maxContextTokens` cap (A:2633-2637), read first
        /// by currentMaxContextTokens (A:2570-2578).
        context_caps: HashMap<(String, String), f64>,
        catalog: Option<f64>,
        cap_sets: Vec<String>,
        /// (messages, output cap at prepare time) per prepared request.
        prepares: Vec<(Vec<JsValue>, Option<f64>)>,
        calibration_clears: usize,
        recovery_prep: PrepReply,
        /// (target, message sigs) per prepare_recovery_compaction.
        recovery_previews: Vec<(String, Vec<String>)>,
    }
    fn key(route: &Route) -> (String, String) {
        (
            route.provider.to_utf8().unwrap(),
            route.model.to_utf8().unwrap(),
        )
    }
    impl Preparation {
        fn current_cap(&self, config_cap: Option<f64>, route: &Route) -> Option<f64> {
            let caps = self.caps.get(&key(route)).cloned().unwrap_or_default();
            let base = caps
                .attempt
                .or(caps.requested)
                .or(caps.session)
                .or(config_cap);
            let mut candidates = Vec::new();
            candidates.extend(base);
            candidates.extend(caps.hard);
            if base.is_some() {
                candidates.extend(self.catalog);
            }
            candidates
                .into_iter()
                .filter(|v| v.is_finite() && *v > 0.0)
                .map(f64::floor)
                .reduce(f64::min)
        }
    }
    impl TurnPreparationPort for Preparation {
        fn current_output_cap(
            &self,
            config: &TurnRuntimeConfig,
            route: &Route,
        ) -> PortResult<Option<f64>> {
            Ok(self.current_cap(config.max_output_tokens, route))
        }
        fn model_output_limit(&self, _: &Route) -> Option<f64> {
            self.catalog
        }
        fn set_output_cap(&mut self, route: &Route, update: OutputCapUpdate) -> PortResult<()> {
            let (provider, model) = key(route);
            self.cap_sets.push(format!("{provider}/{model}:{update:?}"));
            let caps = self.caps.entry((provider, model)).or_default();
            match update {
                OutputCapUpdate::Requested(v) => caps.requested = Some(v),
                OutputCapUpdate::Session(v) => caps.session = Some(v),
                OutputCapUpdate::Attempt(v) => caps.attempt = Some(v),
                OutputCapUpdate::HardCap(v) => caps.hard = Some(v),
            }
            Ok(())
        }
        fn clear_attempt_output_cap(&mut self, route: &Route) {
            if let Some(caps) = self.caps.get_mut(&key(route)) {
                caps.attempt = None;
            }
        }
        fn clear_compaction_calibration(&mut self) {
            self.calibration_clears += 1;
        }
        fn set_context_cap(&mut self, route: &Route, max_context_tokens: f64) -> PortResult<()> {
            let (provider, model) = key(route);
            self.cap_sets.push(format!(
                "{provider}/{model}:Context({max_context_tokens:?})"
            ));
            self.context_caps
                .insert((provider, model), max_context_tokens);
            Ok(())
        }
        fn compaction_token_limits(
            &self,
            config: &TurnRuntimeConfig,
            route: &Route,
            _pre_route: bool,
        ) -> (f64, f64) {
            (
                self.context_caps
                    .get(&key(route))
                    .copied()
                    .or(config.max_context_tokens)
                    .unwrap_or(1_000_000.0),
                config.max_output_tokens.unwrap_or(0.0),
            )
        }
        fn prepare_recovery_compaction(
            &mut self,
            _: &mut dyn HostPort,
            _: &CallControl,
            parameters: RecoveryCompactionPreparation<'_>,
        ) -> PortResult<Option<Box<CompactionIntent>>> {
            let (provider, model) = key(parameters.target);
            self.recovery_previews
                .push((format!("{provider}/{model}"), sigs(parameters.messages)));
            match self.recovery_prep {
                PrepReply::Absent => return Ok(None),
                PrepReply::Advisory => return Err(host_failure("controlled-preview")),
                PrepReply::Fatal => {
                    return Err(host_failure("COMPACTION_BUDGET_CONTRACT_INVALID"));
                }
                PrepReply::Intent => {}
            }
            Ok(Some(Box::new(CompactionIntent {
                session_id: Some(parameters.input.session_id.clone()),
                turn_id: Some(parameters.input.turn_id.clone()),
                stage: BudgetStage::Recovery,
                trigger: CompactionTrigger::Auto,
                messages: parameters.messages.to_vec(),
                max_context_tokens: Some(
                    self.compaction_token_limits(parameters.config, parameters.target, false)
                        .0,
                ),
                reserved_output_tokens: Some(
                    self.current_cap(parameters.config.max_output_tokens, parameters.target)
                        .unwrap_or(0.0),
                ),
                manual_force: None,
                allow_fallback_on_failure: Some(true),
                budget_request: Some(CapturedRequest {
                    route: parameters.target.clone(),
                    normalized: RequestProjection::BoundaryOnly(Vec::new()),
                    wire: PreparedWireRequest::capture(&json(
                        r#"{"provider":"preview","model":"preview","messages":[]}"#,
                    )),
                }),
                preparation: None,
                prepared_handle: None,
                calibration: None,
            })))
        }
        fn prepare(
            &mut self,
            _: &mut dyn HostPort,
            _: &CallControl,
            config: &TurnRuntimeConfig,
            _: &TurnInput,
            messages: &[ProjectedMessage],
        ) -> PortResult<(CapturedRequest, ModelHints)> {
            self.prepares.push((
                messages
                    .iter()
                    .map(|m| m.boundary.value().clone())
                    .collect(),
                self.current_cap(config.max_output_tokens, &route()),
            ));
            Ok((
                CapturedRequest {
                    route: route(),
                    normalized: RequestProjection::BoundaryOnly(Vec::new()),
                    wire: PreparedWireRequest::capture(&json(
                        r#"{"provider":"p1","model":"m1","messages":[]}"#,
                    )),
                },
                ModelHints {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    execution: ExecutionHints {
                        run_id: "run".into(),
                        operation_id: "op".into(),
                        idempotency_key: None,
                        operation_deadline: None,
                    },
                    workspace_id: None,
                    storage_config_version: None,
                    metadata: None,
                    model_override: None,
                },
            ))
        }
        fn project_assembly(&self, boundary: &BoundarySnapshot) -> PortResult<TurnAssembly> {
            let message = boundary
                .value()
                .get("message")
                .cloned()
                .expect("assembled message");
            Ok(TurnAssembly {
                boundary: boundary.clone(),
                message: ProjectedMessage {
                    boundary: BoundarySnapshot::capture(&message),
                    normalized: MessageProjection::BoundaryOnly(vec![
                        crate::ProjectionGap::OtherBoundaryShape,
                    ]),
                },
                usage: None,
                has_repaired_tool_calls: None,
                has_message_end: boundary.value().get("hasMessageEnd")
                    == Some(&JsValue::Bool(true)),
            })
        }
        fn project_tool_calls(&self, calls: &[BoundarySnapshot]) -> PortResult<Vec<ToolCall>> {
            Ok(calls
                .iter()
                .map(|call| {
                    let value = call.value();
                    ToolCall {
                        timeline: None,
                        id: value
                            .get("id")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap(),
                        name: value
                            .get("name")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap(),
                        input: value.get("input").cloned().unwrap_or(JsValue::Undefined),
                        raw: value
                            .get("raw")
                            .map(|raw| ProviderEcho(Arc::new(raw.clone()))),
                    }
                })
                .collect())
        }
        fn project_tool_result(
            &self,
            result: &BoundarySnapshot,
        ) -> PortResult<ToolExecutionResult> {
            let value = result.value();
            let field = |key: &str| -> JsString {
                value
                    .get(key)
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap()
            };
            let error = value.get("error").expect("error object");
            let JsValue::Array(content) = value.get("content").expect("content") else {
                panic!("content array");
            };
            Ok(ToolExecutionResult {
                tool_call_id: field("toolCallId"),
                tool_name: field("toolName"),
                disposition: ToolResultDisposition::Error {
                    code: error
                        .get("code")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap(),
                    message: error
                        .get("message")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap(),
                    fields: JsObject::new(),
                },
                content: content
                    .iter()
                    .map(|block| ToolContent::Text {
                        text: block
                            .get("text")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap(),
                        boundary: BoundarySnapshot::capture(block),
                    })
                    .collect(),
                supplemental_messages: None,
                metadata: value.get("metadata").and_then(JsValue::as_object).cloned(),
                started_at: field("startedAt"),
                completed_at: field("completedAt"),
                boundary: result.clone(),
            })
        }
    }
    struct Events {
        events: Vec<JsValue>,
    }
    impl TurnEventPort for Events {
        fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            self.events.push(event.value().clone());
            Ok(())
        }
    }
    struct Clock {
        uuids: Cell<usize>,
    }
    impl TurnClock for Clock {
        fn now_iso(&self) -> JsString {
            NOW.into()
        }
        fn uuid(&self) -> JsString {
            self.uuids.set(self.uuids.get() + 1);
            format!("uuid-{}", self.uuids.get()).into()
        }
    }
    fn route() -> Route {
        Route {
            provider: "p1".into(),
            model: "m1".into(),
        }
    }
    fn config(cap: Option<f64>) -> TurnRuntimeConfig {
        TurnRuntimeConfig {
            route: route(),
            cwd: "/controlled-workspace".into(),
            system_prompt: None,
            append_system_prompt: None,
            plan_mode_instructions: None,
            runtime_context_surface: RuntimeContextSurface::SystemPrompt,
            max_output_tokens: cap,
            max_context_tokens: Some(32768.0),
            thinking: None,
            tool_choice: None,
            max_context_messages: None,
            stop_on_structured_output: None,
            json_self_correct: Some(false),
            run_mode: None,
            is_subagent: None,
            subagent_model: None,
            permission_mode_before_plan: None,
            permission: TurnPermissionContext {
                mode: PermissionMode::BypassPermissions,
                can_prompt: false,
                bypass_available: true,
                additional_working_directories: vec![],
                rules: None,
            },
            metadata: None,
        }
    }
    fn turn_input() -> TurnInput {
        TurnInput {
            session_id: "reactive-session".into(),
            turn_id: "reactive-turn".into(),
            messages: vec![],
            max_turns: Some(8.0),
            max_budget_usd: None,
            task_budget_usd: None,
            initial_task_budget_spent_usd: None,
            base_permission_mode: None,
            allow_plan_mode_tools: false,
            can_elicit: false,
            model_override: None,
        }
    }
    fn tool_hints() -> ToolHints {
        ToolHints {
            session_id: "reactive-session".into(),
            turn_id: "reactive-turn".into(),
            cwd: "/controlled-workspace".into(),
            permission_mode: PermissionMode::BypassPermissions,
            run_mode: None,
            can_prompt: false,
            can_elicit: None,
            additional_working_directories: vec![],
            context_extensions: JsObject::new(),
            execution: ExecutionHints {
                run_id: "run".into(),
                operation_id: "op".into(),
                idempotency_key: None,
                operation_deadline: None,
            },
        }
    }
    struct Observed {
        result: PortResult<TurnOutput>,
        events: Vec<JsValue>,
        probes: Vec<(bool, usize)>,
        cap_sets: Vec<String>,
        prepares: Vec<(Vec<JsValue>, Option<f64>)>,
        calibration_clears: usize,
        final_cap: Option<f64>,
        captures: usize,
        compactions: Vec<CompactionRecord>,
        persists: Vec<(JsValue, Vec<String>)>,
        recovery_previews: Vec<(String, Vec<String>)>,
    }
    impl Observed {
        fn label(event: &JsValue) -> String {
            let kind = str_of(event, "type").unwrap();
            match kind.as_str() {
                "agent_status" => format!("agent_status:{}", str_of(event, "event").unwrap()),
                "turn_continued" => format!("turn_continued:{}", str_of(event, "reason").unwrap()),
                _ => kind,
            }
        }
        fn labels(&self) -> Vec<String> {
            self.events.iter().map(Self::label).collect()
        }
        /// Labels after the n-th (1-based) assistant_message up to the next
        /// model_request_started (exclusive) or the end of the run.
        fn after_assistant(&self, n: usize) -> Vec<String> {
            let labels = self.labels();
            let start = labels
                .iter()
                .enumerate()
                .filter(|(_, l)| l.as_str() == "assistant_message")
                .nth(n - 1)
                .map(|(i, _)| i + 1)
                .expect("assistant_message present");
            labels[start..]
                .iter()
                .take_while(|l| l.as_str() != "model_request_started")
                .cloned()
                .collect()
        }
        fn events_of(&self, kind: &str) -> Vec<&JsValue> {
            self.events
                .iter()
                .filter(|e| str_of(e, "type").as_deref() == Some(kind))
                .collect()
        }
        fn count(&self, label: &str) -> usize {
            self.labels().iter().filter(|l| l.as_str() == label).count()
        }
        fn output(&self) -> &TurnOutput {
            self.result.as_ref().expect("turn returned")
        }
        fn error_code(&self) -> String {
            str_of(&self.output().result.errors[0].value().clone(), "code").unwrap()
        }
        fn prepared_sigs(&self, n: usize) -> Vec<String> {
            self.prepares[n].0.iter().map(sig).collect()
        }
    }
    fn run(script: Script) -> Observed {
        let mut host = Host {
            replies: script.replies,
            decisions: script.decisions,
            advertise_recovery: script.advertise_recovery,
            advertise_compact: script.advertise_compact,
            advertise_persist: script.advertise_persist,
            compact_replies: script.compact_replies,
            persist_reply: script.persist_reply,
            pulls: 0,
            probes: vec![],
            captures: 0,
            compactions: vec![],
            persists: vec![],
        };
        let mut preparation = Preparation {
            caps: HashMap::new(),
            context_caps: HashMap::new(),
            catalog: script.catalog,
            cap_sets: vec![],
            prepares: vec![],
            calibration_clears: 0,
            recovery_prep: script.recovery_prep,
            recovery_previews: vec![],
        };
        let mut events = Events { events: vec![] };
        let clock = Clock {
            uuids: Cell::new(0),
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let cleanup = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let mut config = config(script.config_cap);
        let input = turn_input();
        let mut hints = tool_hints();
        let result = run_admitted_turn(
            &mut config,
            &input,
            script.initial,
            &mut host,
            &mut preparation,
            &mut events,
            &clock,
            &control,
            &cleanup,
            &mut hints,
        );
        let final_cap = preparation.current_cap(script.config_cap, &route());
        Observed {
            result,
            events: events.events,
            probes: host.probes,
            cap_sets: preparation.cap_sets,
            prepares: preparation.prepares,
            calibration_clears: preparation.calibration_clears,
            final_cap,
            captures: host.captures,
            compactions: host.compactions,
            persists: host.persists,
            recovery_previews: preparation.recovery_previews,
        }
    }
    const TERMINAL: [&str; 4] = [
        "stop_failure",
        "agent_status:model_request_failed",
        "turn_failed",
        "turn_completed",
    ];
    fn assert_terminal_error(o: &Observed, stop: TurnStopReason, code: &str) {
        let result = &o.output().result;
        assert!(matches!(result.kind, TurnResultKind::Error));
        assert_eq!(result.stop_reason, stop);
        assert_eq!(o.error_code(), code);
        // N7: no model-error `continue` changes turnCount.
        assert_eq!(result.turns, 1.0);
    }
    fn assert_success(o: &Observed) {
        let result = &o.output().result;
        assert!(matches!(result.kind, TurnResultKind::Success));
        assert_eq!(result.turns, 1.0);
    }
    fn cap_event(event: &JsValue) -> (String, String, String, Option<f64>, f64, String) {
        (
            str_of(event, "provider").unwrap(),
            str_of(event, "model").unwrap(),
            str_of(event, "cap").unwrap(),
            num_of(event, "previous"),
            num_of(event, "next").unwrap(),
            str_of(event, "reason").unwrap(),
        )
    }
    const ERR_INVALID_AVAILABLE: &str = r#"{"provider":"p1","code":"invalid_request","message":"Controlled","availableOutputTokens":8000}"#;
    const ERR_PTL: &str = r#"{"provider":"p1","code":"prompt_too_long","message":"Controlled"}"#;
    const ERR_IMAGE: &str = r#"{"provider":"p1","code":"image_too_large","message":"Controlled"}"#;
    const ERR_MAX_OUTPUT: &str =
        r#"{"provider":"p2","model":"m2","code":"max_output_reached","message":"Controlled"}"#;

    // ----- profiles -----
    #[test]
    fn p01_adjust_attempt_scope() {
        let o = run(Script::new(
            vec![error_reply(ERR_INVALID_AVAILABLE, 0), Reply::Success],
            vec![adjust(8000.0, Some(OutputLimitScope::Attempt))],
        ));
        assert_eq!(o.probes, vec![(false, 2)]);
        assert_eq!(o.cap_sets, vec!["p1/m1:Attempt(8000.0)"]);
        assert_eq!(
            o.after_assistant(1),
            ["token_cap_adjusted", "turn_continued:model_error"]
        );
        let caps = o.events_of("token_cap_adjusted");
        assert_eq!(caps.len(), 1);
        assert_eq!(
            cap_event(caps[0]),
            (
                "p1".into(),
                "m1".into(),
                "output".into(),
                Some(4000.0),
                8000.0,
                "provider-output-cap".into()
            )
        );
        // N2: the failed assistant is not replayed; request 2 carries 8000.
        assert_eq!(o.prepared_sigs(1), ["user:[text(Controlled request)]"]);
        assert_eq!(o.prepares[1].1, Some(8000.0));
        // A:825: the attempt cap lives for exactly one request.
        assert_eq!(o.final_cap, Some(4000.0));
        assert_eq!(o.calibration_clears, 0);
        assert_success(&o);
    }
    #[test]
    fn p02_adjust_hard_cap_error_target() {
        let o = run(Script::new(
            vec![
                error_reply(
                    r#"{"provider":"p2","model":"m2","code":"invalid_request","message":"Controlled","maxOutputTokens":4096}"#,
                    1,
                ),
                Reply::Success,
            ],
            vec![adjust(4096.0, Some(OutputLimitScope::HardCap))],
        ));
        // Probe sees H + A_err + TR_syn.
        assert_eq!(o.probes, vec![(false, 3)]);
        // N3: hard cap on the error target, then attempt cap on the decision.
        assert_eq!(
            o.cap_sets,
            vec!["p2/m2:HardCap(4096.0)", "p1/m1:Attempt(4096.0)"]
        );
        assert_eq!(
            o.after_assistant(1),
            [
                "tool_results_projected",
                "token_cap_adjusted",
                "turn_continued:model_error"
            ]
        );
        let (provider, model, cap, previous, next, reason) =
            cap_event(o.events_of("token_cap_adjusted")[0]);
        assert_eq!(
            (provider.as_str(), model.as_str(), cap.as_str()),
            ("p2", "m2", "output")
        );
        assert_eq!(
            (previous, next, reason.as_str()),
            (Some(4000.0), 4096.0, "provider-output-cap")
        );
        // N2: neither the assistant nor the synthetic tool_result is replayed.
        assert_eq!(o.prepared_sigs(1), ["user:[text(Controlled request)]"]);
        assert_eq!(o.prepares[1].1, Some(4096.0));
        assert_success(&o);
    }
    #[test]
    fn p02b_falsy_error_target_falls_back_to_decision_route() {
        let o = run(Script::new(
            vec![
                error_reply(
                    r#"{"provider":"","model":null,"code":"invalid_request","message":"Controlled","maxOutputTokens":4096}"#,
                    0,
                ),
                Reply::Success,
            ],
            vec![adjust(4096.0, None)],
        ));
        // scope undefined -> hard cap (OPEN-2); target == decision -> one set.
        assert_eq!(o.cap_sets, vec!["p1/m1:HardCap(4096.0)"]);
        let (provider, model, ..) = cap_event(o.events_of("token_cap_adjusted")[0]);
        assert_eq!((provider.as_str(), model.as_str()), ("p1", "m1"));
        assert_success(&o);
    }
    #[test]
    fn p03_adjust_second_blocked() {
        let o = run(Script::new(
            vec![
                error_reply(ERR_INVALID_AVAILABLE, 0),
                error_reply(ERR_INVALID_AVAILABLE, 0),
            ],
            vec![
                adjust(8000.0, Some(OutputLimitScope::Attempt)),
                adjust(8000.0, Some(OutputLimitScope::Attempt)),
            ],
        ));
        assert_eq!(o.probes, vec![(false, 2), (false, 2)]);
        // N1: the second adjust applies nothing.
        assert_eq!(o.cap_sets.len(), 1);
        assert_eq!(o.events_of("token_cap_adjusted").len(), 1);
        assert_eq!(o.after_assistant(2), TERMINAL);
        assert_eq!(o.count("turn_continued:model_error"), 1);
        assert_terminal_error(&o, TurnStopReason::ModelError, "agent_model_error");
        // No strip on the blocked branch: returned messages keep A_err2.
        assert_eq!(o.output().messages.len(), 2);
    }
    fn five_message_history() -> Vec<ProjectedMessage> {
        vec![
            user("first request"),
            assistant("reply one"),
            user("second request"),
            assistant("reply two"),
            user("third request"),
        ]
    }
    #[test]
    fn p08_truncate_twice_then_give_up() {
        // err1 carries a tool call so the pair (A_err + TR_syn) is a paired
        // tool round that truncation alone would keep: only the strip removes it.
        let mut script = Script::new(
            vec![
                error_reply(ERR_PTL, 1),
                error_reply(ERR_PTL, 0),
                error_reply(ERR_PTL, 0),
            ],
            vec![
                truncate(0.5, "ptl-first-attempt"),
                truncate(0.25, "ptl-second-attempt"),
                give_up(),
            ],
        );
        script.initial = five_message_history();
        let o = run(script);
        // N5: hasAttemptedCompact false, then true, true. N10: second truncate applied.
        assert_eq!(o.probes, vec![(false, 7), (true, 4), (true, 2)]);
        assert!(o.cap_sets.is_empty());
        assert!(o.events_of("token_cap_adjusted").is_empty());
        assert_eq!(
            o.after_assistant(1),
            ["tool_results_projected", "turn_continued:model_error"]
        );
        assert_eq!(o.after_assistant(2), ["turn_continued:model_error"]);
        assert_eq!(o.after_assistant(3), TERMINAL);
        // N2 + OPEN-5: A_err stripped first, then keepRatio over valid history.
        assert_eq!(
            o.prepared_sigs(1),
            [
                "user:[text(second request)]",
                "assistant:[text(reply two)]",
                "user:[text(third request)]"
            ]
        );
        assert_eq!(o.prepared_sigs(2), ["user:[text(third request)]"]);
        assert_eq!(o.calibration_clears, 2);
        assert_terminal_error(&o, TurnStopReason::PromptTooLong, "agent_prompt_too_long");
    }
    #[test]
    fn p09_strip_images_once() {
        let mut script = Script::new(
            vec![error_reply(ERR_IMAGE, 0), error_reply(ERR_IMAGE, 0)],
            vec![strip_images(), strip_images()],
        );
        script.initial = vec![
            blocks(
                "user",
                &format!(
                    r#"{},{{"type":"image","source":"base64","data":"AAA","mimeType":"image/png"}}"#,
                    text("look")
                ),
            ),
            blocks("assistant", &tc("c1")),
            blocks(
                "user",
                r#"{"type":"tool_result","toolCallId":"c1","isError":false,"content":[{"type":"image","source":"url","data":"https://x/y.png","mimeType":"image/png"},{"type":"text","text":"ok"}]}"#,
            ),
        ];
        let o = run(script);
        assert_eq!(o.probes, vec![(false, 4), (false, 4)]);
        assert!(o.cap_sets.is_empty());
        assert_eq!(o.after_assistant(1), ["turn_continued:model_error"]);
        assert_eq!(
            o.prepared_sigs(1),
            [
                format!("user:[text(look),text({IMAGE_REMOVED_TEXT})]"),
                "assistant:[tool_call#c1]".to_string(),
                format!("user:[tool_result#c1{{text({IMAGE_REMOVED_TEXT}),text(ok)}}]"),
            ]
        );
        assert_eq!(o.calibration_clears, 1);
        // Second strip is guard-blocked: terminal, no further continue.
        assert_eq!(o.after_assistant(2), TERMINAL);
        assert_eq!(o.count("turn_continued:model_error"), 1);
        assert_terminal_error(&o, TurnStopReason::ModelError, "agent_model_error");
    }
    #[test]
    fn p10_max_output_phase_a_bump() {
        let o = run(Script::new(
            vec![error_reply(ERR_MAX_OUTPUT, 0), Reply::Success],
            vec![give_up()],
        ));
        assert_eq!(o.probes, vec![(false, 2)]);
        // Phase A uses the decision route, never modelErrorTarget (p2/m2).
        assert_eq!(o.cap_sets, vec!["p1/m1:Requested(8000.0)"]);
        assert_eq!(
            o.after_assistant(1),
            ["token_cap_adjusted", "turn_continued:model_error"]
        );
        assert_eq!(
            cap_event(o.events_of("token_cap_adjusted")[0]),
            (
                "p1".into(),
                "m1".into(),
                "output".into(),
                Some(4000.0),
                8000.0,
                "max-output-retry-bump".into()
            )
        );
        // N2: Phase A strips the pair; N13: requested cap persists for the run.
        assert_eq!(o.prepared_sigs(1), ["user:[text(Controlled request)]"]);
        assert_eq!(o.prepares[1].1, Some(8000.0));
        assert_eq!(o.final_cap, Some(8000.0));
        assert_eq!(o.calibration_clears, 0);
        assert_success(&o);
    }
    #[test]
    fn p10b_phase_a_without_bump_consumes_guard_then_phase_b_then_adjust_blocked() {
        // N6: current (16000) >= catalog cap (16000) -> no bump; OPEN-3: the
        // consumed guard blocks a later adjust_output_and_retry.
        let mut script = Script::new(
            vec![
                error_reply(ERR_MAX_OUTPUT, 0),
                error_reply(ERR_INVALID_AVAILABLE, 0),
            ],
            vec![give_up(), adjust(8000.0, Some(OutputLimitScope::Attempt))],
        );
        script.config_cap = Some(16000.0);
        let o = run(script);
        assert_eq!(o.probes, vec![(false, 2), (false, 3)]);
        assert!(o.cap_sets.is_empty());
        assert!(o.events_of("token_cap_adjusted").is_empty());
        assert_eq!(o.after_assistant(1), ["turn_continued:model_error"]);
        // Phase B keeps A_err and appends the transient prompt (no strip).
        let request_2 = &o.prepares[1].0;
        assert_eq!(request_2.len(), 3);
        assert_eq!(str_of(&request_2[1], "role").as_deref(), Some("assistant"));
        let prompt = &request_2[2];
        assert_eq!(
            sig(prompt),
            format!("user:[text({MAX_OUTPUT_PROMPT})]+meta")
        );
        let metadata = prompt.get("metadata").unwrap();
        assert_eq!(metadata.get("synthetic"), Some(&JsValue::Bool(true)));
        assert_eq!(metadata.get("transient"), Some(&JsValue::Bool(true)));
        assert_eq!(
            str_of(metadata, "purpose").as_deref(),
            Some("max_output_recovery")
        );
        assert!(str_of(metadata, "transientId").is_some());
        assert_eq!(o.after_assistant(2), TERMINAL);
        assert_terminal_error(&o, TurnStopReason::ModelError, "agent_model_error");
    }
    #[test]
    fn p11_max_output_phase_b_fifty_times_then_phase_c() {
        let mut script = Script::new(
            vec![error_reply(ERR_MAX_OUTPUT, 0); 51],
            vec![give_up(); 51],
        );
        script.config_cap = None;
        script.catalog = None;
        let o = run(script);
        assert_eq!(o.probes.len(), 51);
        assert!(o.probes.iter().all(|(compact, _)| !compact));
        assert!(o.cap_sets.is_empty());
        assert!(o.events_of("token_cap_adjusted").is_empty());
        assert_eq!(o.count("turn_continued:model_error"), 50);
        for k in 1..=50 {
            assert_eq!(o.after_assistant(k), ["turn_continued:model_error"]);
            // N2/N12: H + k truncated assistants + exactly one live prompt.
            let request = &o.prepares[k].0;
            assert_eq!(request.len(), k + 2, "request {}", k + 1);
            assert_eq!(str_of(&request[k], "role").as_deref(), Some("assistant"));
            let prompts: Vec<_> = request
                .iter()
                .filter(|m| {
                    m.get("metadata")
                        .and_then(|md| str_of(md, "purpose"))
                        .as_deref()
                        == Some("max_output_recovery")
                })
                .collect();
            assert_eq!(prompts.len(), 1);
            assert_eq!(
                sig(prompts[0]),
                format!("user:[text({MAX_OUTPUT_PROMPT})]+meta")
            );
            assert_eq!(o.probes[k].1, k + 2, "probe {} sees H + assistants", k + 1);
        }
        assert_eq!(o.after_assistant(51), TERMINAL);
        assert_terminal_error(&o, TurnStopReason::ModelError, "agent_model_error");
        // Returned messages: H + 51 assistants, no transient prompt.
        let returned = &o.output().messages;
        assert_eq!(returned.len(), 52);
        assert!(
            returned
                .iter()
                .all(|m| m.boundary.value().get("metadata").is_none())
        );
    }
    #[test]
    fn p12_reasoning_content_once() {
        let error = format!(
            r#"{{"provider":"p1","code":"invalid_request","message":"{REASONING_MESSAGE}"}}"#
        );
        let mut script = Script::new(
            vec![error_reply(&error, 1), error_reply(&error, 1)],
            vec![give_up()],
        );
        script.initial = vec![user("task"), assistant("earlier"), user("next")];
        let o = run(script);
        // N8: no projection, no probe, no strip before the reasoning retry.
        assert_eq!(o.after_assistant(1), ["turn_continued:model_error"]);
        assert_eq!(
            o.prepared_sigs(1),
            [
                "user:[text(task)]",
                "assistant:[thinking(|),text(earlier)]",
                "user:[text(next)]",
                "assistant:[thinking(|),tool_call#call-1-0]",
            ]
        );
        // err2: projected, probed once (H' + A_err1' + A_err2 + TR_syn2), terminal.
        assert_eq!(o.probes, vec![(false, 6)]);
        let mut expected = vec!["tool_results_projected"];
        expected.extend(TERMINAL);
        assert_eq!(o.after_assistant(2), expected);
        assert!(o.cap_sets.is_empty());
        assert_terminal_error(&o, TurnStopReason::ModelError, "agent_model_error");
    }
    // ----- negative controls not already carried by a profile -----
    #[test]
    fn n4_give_up_unknown_kind_probe_failure_and_absent_port_fall_through() {
        for (decision, advertised, probes) in [
            (give_up(), true, 1),
            (
                Decision::Raw(r#"{"type":"future_kind","reason":"x"}"#),
                true,
                1,
            ),
            (Decision::Raw("null"), true, 1),
            (Decision::Failure, true, 1),
            (give_up(), false, 0),
        ] {
            let mut script =
                Script::new(vec![error_reply(ERR_INVALID_AVAILABLE, 0)], vec![decision]);
            script.advertise_recovery = advertised;
            let o = run(script);
            assert_eq!(o.probes.len(), probes);
            assert_eq!(o.count("turn_continued:model_error"), 0);
            assert!(o.cap_sets.is_empty());
            assert_eq!(o.prepares.len(), 1);
            assert_eq!(o.after_assistant(1), TERMINAL);
            assert_terminal_error(&o, TurnStopReason::ModelError, "agent_model_error");
        }
    }
    // ----- compact_and_retry (A:1476-1580; contract P04-P07, N2/N5/N9) -----
    const COMPACTED: &str = r#"{"type":"compacted","tier":"summary","messages":[{"role":"user","content":[{"type":"text","text":"compacted summary"}]},{"role":"user","content":[{"type":"text","text":"third request"}]}],"result":{"compactionId":"compact-1","trigger":"auto","preTokens":100,"messagesSummarized":2},"snapshot":{"tokens":25,"ratio":0.25,"maxContextTokens":100000}}"#;
    const COMPACTED_WITH_ERROR: &str = r#"{"type":"compacted","tier":"emergency","error":"context_overflow_after_emergency_compaction","messages":[{"role":"user","content":[{"type":"text","text":"compacted summary"}]},{"role":"user","content":[{"type":"text","text":"third request"}]}],"result":{"compactionId":"compact-1","trigger":"auto","preTokens":100,"messagesSummarized":2},"snapshot":{"tokens":25,"ratio":0.25,"maxContextTokens":100000}}"#;
    const SKIPPED: &str =
        r#"{"type":"skipped","snapshot":{"tokens":5000,"ratio":0.9,"maxContextTokens":100000}}"#;
    fn five_sigs() -> Vec<String> {
        sigs(&five_message_history())
    }
    fn replacement_sigs() -> Vec<String> {
        vec![
            "user:[text(compacted summary)]".into(),
            "user:[text(third request)]".into(),
        ]
    }
    /// truncateHeadKeepRatio(five_message_history, 0.5) per CE:780-815 (the
    /// same hand-derived list as p08's second request).
    fn truncated_half_sigs() -> Vec<String> {
        vec![
            "user:[text(second request)]".into(),
            "assistant:[text(reply two)]".into(),
            "user:[text(third request)]".into(),
        ]
    }
    /// err1 carries one tool call so the strip (A:1511) is observable: the
    /// probe sees H(5) + A_err + TR_syn = 7, the compaction intent sees 5.
    fn compact_script(decisions: Vec<Decision>, compact_replies: Vec<CompactReply>) -> Script {
        let mut replies = vec![error_reply(ERR_PTL, 1)];
        replies.extend(std::iter::repeat_n(
            error_reply(ERR_PTL, 0),
            decisions.len() - 1,
        ));
        replies.push(Reply::Success);
        let mut script = Script::new(replies, decisions);
        script.compact_replies = compact_replies;
        script.initial = five_message_history();
        script
    }
    const COMPACT_BRANCH_EVENTS: [&str; 3] = [
        "tool_results_projected",
        "token_cap_adjusted",
        "turn_continued:model_error",
    ];
    fn assert_context_cap_event(o: &Observed, route: (&str, &str), previous: f64, next: f64) {
        let caps = o.events_of("token_cap_adjusted");
        assert_eq!(caps.len(), 1);
        assert_eq!(
            cap_event(caps[0]),
            (
                route.0.into(),
                route.1.into(),
                "context".into(),
                Some(previous),
                next,
                "provider-context-cap".into()
            )
        );
    }
    fn recovery_record(target: &str, max_context_tokens: f64, reserved: f64) -> CompactionRecord {
        CompactionRecord {
            stage: BudgetStage::Recovery,
            target: target.into(),
            max_context_tokens: Some(max_context_tokens),
            reserved_output_tokens: Some(reserved),
            allow_fallback_on_failure: Some(true),
            messages: five_sigs(),
        }
    }
    #[test]
    fn p04_compact_compacted() {
        let o = run(compact_script(
            vec![compact()],
            vec![CompactReply::Boundary(COMPACTED)],
        ));
        assert_eq!(o.probes, vec![(false, 7)]);
        // A:1480 only the context cap; previous = config 32768 read before it.
        assert_eq!(o.cap_sets, vec!["p1/m1:Context(100000.0)"]);
        assert_eq!(o.after_assistant(1), COMPACT_BRANCH_EVENTS);
        assert_context_cap_event(&o, ("p1", "m1"), 32768.0, 100_000.0);
        // A:1514-1544: recovery intent on the target with the post-set limit
        // and the stripped window (N2).
        assert_eq!(
            o.recovery_previews,
            vec![("p1/m1".to_string(), five_sigs())]
        );
        assert_eq!(
            o.compactions,
            vec![recovery_record("p1/m1", 100_000.0, 4000.0)]
        );
        // A:1547 persistCompactSnapshot: boundary + marked replacement.
        assert_eq!(o.persists.len(), 1);
        let (boundary, marked) = &o.persists[0];
        assert_eq!(str_of(boundary, "kind").as_deref(), Some("compact"));
        let metadata = boundary.get("compactMetadata").unwrap();
        assert_eq!(
            str_of(metadata, "compactionId").as_deref(),
            Some("compact-1")
        );
        assert!(metadata.get("extra").unwrap().get("error").is_none());
        assert_eq!(
            marked,
            &replacement_sigs()
                .iter()
                .map(|s| format!("{s}+meta"))
                .collect::<Vec<_>>()
        );
        // A:1546: request 2 is built from compact.messages; A:1547 calibration
        // cleared exactly once; no truncation (N9).
        assert_eq!(o.prepared_sigs(1), replacement_sigs());
        assert_eq!(o.prepares.len(), 2);
        assert_eq!(o.calibration_clears, 1);
        assert_eq!(o.count("turn_continued:auto_compact"), 0);
        assert_eq!(sigs(&o.output().messages)[..2], replacement_sigs()[..]);
        assert_success(&o);

        // Persistence not advertised: replacement installed, no commit.
        let mut script = compact_script(vec![compact()], vec![CompactReply::Boundary(COMPACTED)]);
        script.advertise_persist = false;
        let o = run(script);
        assert_eq!(o.compactions.len(), 1);
        assert!(o.persists.is_empty());
        assert_eq!(o.prepared_sigs(1), replacement_sigs());
        assert_eq!(o.after_assistant(1), COMPACT_BRANCH_EVENTS);
        assert_success(&o);
    }
    #[test]
    fn n5_probe_after_compact_carries_has_attempted_compact() {
        let o = run(compact_script(
            vec![compact(), give_up()],
            vec![CompactReply::Boundary(COMPACTED)],
        ));
        // Second probe: hasAttemptedCompact true (A:1572) over replacement + A_err2.
        assert_eq!(o.probes, vec![(false, 7), (true, 3)]);
        assert_eq!(o.compactions.len(), 1);
        assert_eq!(o.cap_sets.len(), 1);
        assert_eq!(o.after_assistant(1), COMPACT_BRANCH_EVENTS);
        assert_eq!(o.after_assistant(2), TERMINAL);
        assert_terminal_error(&o, TurnStopReason::PromptTooLong, "agent_prompt_too_long");
    }
    #[test]
    fn p05_compact_with_output_cap() {
        let mut script = Script::new(
            vec![
                error_reply(
                    r#"{"provider":"p2","model":"m2","code":"invalid_request","message":"Controlled","availableOutputTokens":1000,"maxContextTokens":64000}"#,
                    0,
                ),
                Reply::Success,
            ],
            vec![compact_with_output(64000.0, 4096.0)],
        );
        script.compact_replies = vec![CompactReply::Boundary(COMPACTED)];
        let o = run(script);
        assert_eq!(o.probes, vec![(false, 2)]);
        // A:1480 context cap on the target; A:1495-1498 attempt cap on the
        // target and on the decision route (different).
        assert_eq!(
            o.cap_sets,
            vec![
                "p2/m2:Context(64000.0)",
                "p2/m2:Attempt(4096.0)",
                "p1/m1:Attempt(4096.0)"
            ]
        );
        assert_eq!(
            o.after_assistant(1),
            [
                "token_cap_adjusted",
                "token_cap_adjusted",
                "turn_continued:model_error"
            ]
        );
        let caps = o.events_of("token_cap_adjusted");
        assert_eq!(
            cap_event(caps[0]),
            (
                "p2".into(),
                "m2".into(),
                "context".into(),
                Some(32768.0),
                64000.0,
                "provider-available-output-too-small".into()
            )
        );
        assert_eq!(
            cap_event(caps[1]),
            (
                "p2".into(),
                "m2".into(),
                "output".into(),
                Some(4000.0),
                4096.0,
                "provider-available-output-too-small".into()
            )
        );
        // A:1514-1515: limits read after the sets, on the target.
        assert_eq!(o.compactions.len(), 1);
        let record = &o.compactions[0];
        assert_eq!(record.stage, BudgetStage::Recovery);
        assert_eq!(record.target, "p2/m2");
        assert_eq!(record.max_context_tokens, Some(64000.0));
        assert_eq!(record.reserved_output_tokens, Some(4096.0));
        assert_eq!(record.messages, ["user:[text(Controlled request)]"]);
        // Request 2 carries the attempt cap on the decision route (N2 too).
        assert_eq!(o.prepared_sigs(1), replacement_sigs());
        assert_eq!(o.prepares[1].1, Some(4096.0));
        assert_eq!(o.final_cap, Some(4000.0));
        assert_success(&o);
    }
    #[test]
    fn p06_compact_fallback_truncate_three_variants() {
        // (a) not compacted (A:1558-1561); (b) non-fatal host failure
        // (A:1562-1567); (b') non-fatal preview failure inside the try;
        // (c) try_auto_compact not advertised (A:1568-1571).
        let skipped = compact_script(vec![compact()], vec![CompactReply::Boundary(SKIPPED)]);
        let failed = compact_script(
            vec![compact()],
            vec![CompactReply::Failure("controlled-compact")],
        );
        let mut preview_failed = compact_script(vec![compact()], vec![]);
        preview_failed.recovery_prep = PrepReply::Advisory;
        let mut preview_absent = compact_script(vec![compact()], vec![]);
        preview_absent.recovery_prep = PrepReply::Absent;
        let mut absent = compact_script(vec![compact()], vec![]);
        absent.advertise_compact = false;
        for (label, script, calls, previews) in [
            ("skipped", skipped, 1, 1),
            ("failed", failed, 1, 1),
            ("preview_failed", preview_failed, 0, 1),
            ("preview_absent", preview_absent, 0, 1),
            ("absent", absent, 0, 0),
        ] {
            let o = run(script);
            assert_eq!(o.probes, vec![(false, 7)], "{label}");
            assert_eq!(o.cap_sets, vec!["p1/m1:Context(100000.0)"], "{label}");
            assert_eq!(o.after_assistant(1), COMPACT_BRANCH_EVENTS, "{label}");
            assert_context_cap_event(&o, ("p1", "m1"), 32768.0, 100_000.0);
            assert_eq!(o.compactions.len(), calls, "{label}");
            assert_eq!(o.recovery_previews.len(), previews, "{label}");
            assert!(o.persists.is_empty(), "{label}");
            // Strip first (N2), then truncateHeadKeepRatio(0.5) + clear.
            assert_eq!(o.prepared_sigs(1), truncated_half_sigs(), "{label}");
            assert_eq!(o.calibration_clears, 1, "{label}");
            assert_success(&o);
        }
    }
    #[test]
    fn p07_compact_error_terminal() {
        let o = run(compact_script(
            vec![compact()],
            vec![CompactReply::Boundary(COMPACTED_WITH_ERROR)],
        ));
        assert_eq!(o.probes, vec![(false, 7)]);
        assert_eq!(o.cap_sets, vec!["p1/m1:Context(100000.0)"]);
        assert_eq!(
            o.compactions,
            vec![recovery_record("p1/m1", 100_000.0, 4000.0)]
        );
        // A:1547 persistence precedes the A:1549-1556 terminal.
        assert_eq!(o.persists.len(), 1);
        let extra = o.persists[0]
            .0
            .get("compactMetadata")
            .unwrap()
            .get("extra")
            .unwrap();
        assert_eq!(
            str_of(extra, "error").as_deref(),
            Some("context_overflow_after_emergency_compaction")
        );
        let mut expected = vec!["tool_results_projected", "token_cap_adjusted"];
        expected.extend(TERMINAL);
        assert_eq!(o.after_assistant(1), expected);
        assert_eq!(o.count("turn_continued:model_error"), 0);
        assert_eq!(o.count("turn_continued:auto_compact"), 0);
        // captureTurn(true), no second request, replacement returned.
        assert_eq!(o.captures, 1);
        assert_eq!(o.prepares.len(), 1);
        assert_eq!(o.calibration_clears, 1);
        assert_eq!(sigs(&o.output().messages), replacement_sigs());
        assert_terminal_error(
            &o,
            TurnStopReason::PromptTooLong,
            "agent_context_recovery_failed",
        );
    }
    #[test]
    fn n9_compact_guard_blocked_after_truncate_falls_through() {
        // After a truncate, hasAttemptedCompact is true: Source's guard (A:1476)
        // blocks compact_and_retry; no cap, no event, no try_auto_compact.
        let o = run(Script::new(
            vec![error_reply(ERR_PTL, 0), error_reply(ERR_PTL, 0)],
            vec![truncate(0.5, "ptl-first-attempt"), compact()],
        ));
        assert_eq!(o.probes, vec![(false, 2), (true, 2)]);
        assert!(o.cap_sets.is_empty());
        assert!(o.events_of("token_cap_adjusted").is_empty());
        assert!(o.compactions.is_empty());
        assert!(o.recovery_previews.is_empty());
        assert_eq!(o.count("turn_continued:model_error"), 1);
        assert_eq!(o.after_assistant(2), TERMINAL);
        assert_terminal_error(&o, TurnStopReason::PromptTooLong, "agent_prompt_too_long");
    }
    #[test]
    fn fatal_compaction_errors_propagate_not_truncate() {
        // (a) persistence failure (A:2697-2704 CompactionPersistenceError).
        let mut script = compact_script(vec![compact()], vec![CompactReply::Boundary(COMPACTED)]);
        script.persist_reply = PersistReply::Failure;
        let o = run(script);
        assert!(
            matches!(&o.result, Err(PortError::HostFailure { code, .. }) if code.equals_str("controlled-persist"))
        );
        assert_eq!(o.compactions.len(), 1);
        assert_eq!(o.persists.len(), 1);
        assert_eq!(o.prepares.len(), 1);
        assert_eq!(o.count("turn_continued:model_error"), 0);
        // (b) budget-contract error from try_auto_compact (A:101-128).
        let o = run(compact_script(
            vec![compact()],
            vec![CompactReply::Failure("COMPACTION_BUDGET_CONTRACT_INVALID")],
        ));
        assert!(
            matches!(&o.result, Err(PortError::HostFailure { code, .. }) if code.equals_str("COMPACTION_BUDGET_CONTRACT_INVALID"))
        );
        assert_eq!(o.compactions.len(), 1);
        assert_eq!(o.prepares.len(), 1);
        assert_eq!(o.count("turn_continued:model_error"), 0);
        // (c) budget-contract error from the preview (createBudgetRequest).
        let mut script = compact_script(vec![compact()], vec![]);
        script.recovery_prep = PrepReply::Fatal;
        let o = run(script);
        assert!(matches!(&o.result, Err(PortError::HostFailure { .. })));
        assert!(o.compactions.is_empty());
        assert_eq!(o.prepares.len(), 1);
    }

    // ----- pure helper oracles (verifier fixtures) -----
    fn th(input: Vec<ProjectedMessage>, ratio: f64) -> Vec<String> {
        sigs(&truncate_head_preserving_checkpoint(&input, ratio).unwrap())
    }
    #[test]
    fn truncate_head_preserving_checkpoint_fixtures_th1_to_th12() {
        let four = || {
            vec![
                user("Fix the bug"),
                assistant("ok"),
                user("now add tests"),
                assistant("done"),
            ]
        };
        let cont = cont_sig();
        // TH1
        assert_eq!(
            th(four(), 0.5),
            [
                "user:[text(now add tests)]",
                "assistant:[text(done)]",
                &cont
            ]
        );
        // TH2
        assert_eq!(
            th(four(), 0.25),
            [
                "user:[text(now add tests)]",
                "assistant:[text(done)]",
                &cont
            ]
        );
        // TH3
        let th3 = vec![
            user("Refactor module"),
            blocks("assistant", &tc("c1")),
            blocks("user", &tr("c1")),
            blocks("assistant", &tc("c2")),
            blocks("user", &tr("c2")),
            assistant("Finished"),
        ];
        assert_eq!(
            th(th3, 0.5),
            [
                "user:[text(Refactor module)]",
                "assistant:[tool_call#c2]",
                "user:[tool_result#c2{text(r)}]",
                "assistant:[text(Finished)]",
                &cont
            ]
        );
        // TH4
        let th4 = vec![
            user("Run it"),
            blocks("assistant", &tc("c1")),
            blocks("user", &tr("c1")),
            blocks("assistant", &format!("{},{}", text("Also"), tc("c2"))),
            blocks("user", &tr("c2")),
            blocks("assistant", &tc("c3")),
            blocks("assistant", &format!("{},{}", text("Running c4"), tc("c4"))),
        ];
        assert_eq!(
            th(th4, 0.5),
            ["user:[text(Run it)]", "assistant:[text(Running c4)]", &cont]
        );
        // TH5
        let th5 = vec![
            user("Run it"),
            blocks("assistant", &format!("{},{}", text("sure"), tc("c1"))),
            blocks("user", &tr("c1")),
            blocks("assistant", &tc("c2")),
            blocks("user", &tr("c2")),
        ];
        assert_eq!(th(th5, 0.25), ["user:[text(Run it)]"]);
        // TH6
        let th6 = vec![
            user("<compact-boundary id=\\\"1\\\"/>"),
            assistant(&format!("{COMPACT_SUMMARY_PREFIX} Summary.")),
            user("Continue task"),
            assistant("ok"),
            user("more"),
            assistant("done"),
        ];
        assert_eq!(
            th(th6, 0.5),
            [
                "user:[text(<compact-boundary id=\"1\"/>)]",
                &format!("assistant:[text({COMPACT_SUMMARY_PREFIX} Summary.)]"),
                "user:[text(more)]",
                "assistant:[text(done)]",
                &cont
            ]
        );
        // TH7, TH8, TH9
        let three = || vec![user("a"), assistant("b"), user("c")];
        assert_eq!(th(three(), 0.0), ["user:[text(c)]"]);
        assert_eq!(
            th(three(), 1.5),
            ["user:[text(a)]", "assistant:[text(b)]", "user:[text(c)]"]
        );
        assert_eq!(th(three(), f64::NAN), ["user:[text(c)]"]);
        // TH10
        assert!(th(vec![], 0.5).is_empty());
        // TH11
        let th11 = vec![
            message(
                r#"{"role":"user","content":[{"type":"text","text":"x"}],"metadata":{"synthetic":true}}"#,
            ),
            assistant("y"),
        ];
        assert_eq!(th(th11, 0.5), ["assistant:[text(y)]", &cont]);
        // TH12
        let th12 = vec![user("<compact-boundary/>"), user("Do X"), assistant("ok")];
        assert_eq!(
            th(th12, 0.5),
            ["user:[text(Do X)]", "assistant:[text(ok)]", &cont]
        );
    }
    #[test]
    fn add_empty_reasoning_content_markers_fixtures_rc1_to_rc8() {
        let rc = |input: Vec<ProjectedMessage>| {
            sigs(&add_empty_reasoning_content_markers(&input).unwrap())
        };
        let empty_assistant = || blocks("assistant", "");
        // RC1
        assert_eq!(
            rc(vec![user("hi"), empty_assistant()]),
            ["user:[text(hi)]", "assistant:[]"]
        );
        // RC2
        assert_eq!(
            rc(vec![
                user("hi"),
                empty_assistant(),
                user("again"),
                assistant("y")
            ]),
            [
                "user:[text(hi)]",
                "assistant:[thinking(|)]",
                "user:[text(again)]",
                "assistant:[thinking(|),text(y)]"
            ]
        );
        // RC3
        let rc3 = vec![
            user("hi"),
            blocks(
                "assistant",
                r#"{"type":"thinking","text":"abc","reasoningContent":""},{"type":"text","text":"x"}"#,
            ),
            user("q"),
            empty_assistant(),
        ];
        assert_eq!(rc(rc3.clone()), sigs(&rc3));
        // RC4
        let rc4 = vec![
            user("hi"),
            blocks(
                "assistant",
                r#"{"type":"thinking","text":"abc"},{"type":"text","text":"x"}"#,
            ),
            user("q"),
            empty_assistant(),
        ];
        assert_eq!(rc(rc4.clone()), sigs(&rc4));
        // RC5
        let rc5 = vec![
            user("hi"),
            blocks(
                "assistant",
                r#"{"type":"thinking","text":""},{"type":"text","text":"x"}"#,
            ),
            user("q"),
            empty_assistant(),
        ];
        assert_eq!(
            rc(rc5),
            [
                "user:[text(hi)]",
                "assistant:[thinking(|),thinking(|-),text(x)]",
                "user:[text(q)]",
                "assistant:[]"
            ]
        );
        // RC6
        let rc6 = vec![
            blocks(
                "user",
                r#"{"type":"thinking","text":""},{"type":"text","text":"hi"}"#,
            ),
            assistant("x"),
        ];
        assert_eq!(
            rc(rc6),
            [
                "user:[thinking(|-),text(hi)]",
                "assistant:[thinking(|),text(x)]"
            ]
        );
        // RC7
        let rc7 = vec![
            user("hi"),
            blocks(
                "assistant",
                r#"{"type":"thinking","text":"","reasoningContent":"r"},{"type":"text","text":"x"}"#,
            ),
        ];
        assert_eq!(rc(rc7.clone()), sigs(&rc7));
        // RC8
        let rc8 = vec![
            user("hi"),
            blocks("assistant", &tc("c1")),
            blocks("user", &tr("c1")),
            empty_assistant(),
        ];
        assert_eq!(
            rc(rc8),
            [
                "user:[text(hi)]",
                "assistant:[thinking(|),tool_call#c1]",
                "user:[tool_result#c1{text(r)}]",
                "assistant:[]"
            ]
        );
        // Marker key order and exact shape.
        let marked = add_empty_reasoning_content_markers(&[assistant("x"), user("u")]).unwrap();
        let JsValue::Array(content) = marked[0].boundary.value().get("content").unwrap() else {
            panic!("content");
        };
        assert_eq!(
            pilotdeck_protocol::stringify_json(&content[0])
                .unwrap()
                .unwrap(),
            r#"{"type":"thinking","text":"","reasoningContent":""}"#
        );
    }
    #[test]
    fn strip_images_from_messages_fixtures_si1_to_si3() {
        let si = |input: Vec<ProjectedMessage>| sigs(&strip_images_from_messages(&input).unwrap());
        // SI1
        let si1 = vec![blocks(
            "user",
            r#"{"type":"image","source":"base64","data":"AAA","mimeType":"image/png"},{"type":"text","text":"see"}"#,
        )];
        assert_eq!(
            si(si1),
            [format!("user:[text({IMAGE_REMOVED_TEXT}),text(see)]")]
        );
        // SI2 (other tool_result fields preserved)
        let si2 = vec![
            blocks("assistant", &tc("c1")),
            blocks(
                "user",
                r#"{"type":"tool_result","toolCallId":"c1","isError":false,"content":[{"type":"text","text":"ok"},{"type":"image","source":"url","data":"https://x/y.png","mimeType":"image/png"}]}"#,
            ),
        ];
        let stripped = strip_images_from_messages(&si2).unwrap();
        assert_eq!(
            sigs(&stripped),
            [
                "assistant:[tool_call#c1]".to_string(),
                format!("user:[tool_result#c1{{text(ok),text({IMAGE_REMOVED_TEXT})}}]")
            ]
        );
        let JsValue::Array(content) = stripped[1].boundary.value().get("content").unwrap() else {
            panic!("content");
        };
        assert_eq!(content[0].get("isError"), Some(&JsValue::Bool(false)));
        // SI3
        let si3 = vec![assistant("no images"), blocks("user", &tr("c2"))];
        assert_eq!(si(si3.clone()), sigs(&si3));
    }
    #[test]
    fn is_missing_reasoning_content_error_three_positive_five_negative() {
        let check = |message: &str| {
            let mut error = JsObject::new();
            error.insert("message".into(), JsValue::String(message.into()));
            requires_reasoning_content_retry(&BoundarySnapshot::capture(&JsValue::Object(error)))
                .unwrap()
        };
        assert!(check(REASONING_MESSAGE));
        assert!(check(
            "THINKING MODE: missing REASONING_CONTENT; pass back the field."
        ));
        assert!(check(
            "In thinking\tmode the reasoning_content block was not passed  back."
        ));
        assert!(!check("reasoning_content is required"));
        assert!(!check(
            "thinking mode requires reasoning_contents to be passed back"
        ));
        assert!(!check(
            "reasoning_content must be passed back in thinkingmode"
        ));
        assert!(!check(
            "reasoning content is required in thinking mode and must be passed back"
        ));
        assert!(!check("reasoning_content in thinking mode: passback"));
    }
    #[test]
    fn model_error_target_js_or_fallback() {
        let target = |error: &str| {
            let route =
                model_error_target(&BoundarySnapshot::capture(&json(error)), &route()).unwrap();
            (
                route.provider.to_utf8().unwrap(),
                route.model.to_utf8().unwrap(),
            )
        };
        assert_eq!(
            target(r#"{"provider":"p2","model":"m2"}"#),
            ("p2".into(), "m2".into())
        );
        assert_eq!(target(r#"{"provider":""}"#), ("p1".into(), "m1".into()));
        assert_eq!(
            target(r#"{"provider":"p2","model":""}"#),
            ("p2".into(), "m1".into())
        );
        assert_eq!(
            target(r#"{"provider":null,"model":false}"#),
            ("p1".into(), "m1".into())
        );
    }
}
