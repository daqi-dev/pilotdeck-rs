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
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
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
            has_message_end: boundary.value().get("hasMessageEnd") == Some(&JsValue::Bool(true)),
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
    fn project_tool_result(&self, result: &BoundarySnapshot) -> PortResult<ToolExecutionResult> {
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
    let error =
        format!(r#"{{"provider":"p1","code":"invalid_request","message":"{REASONING_MESSAGE}"}}"#);
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
        let mut script = Script::new(vec![error_reply(ERR_INVALID_AVAILABLE, 0)], vec![decision]);
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
    let rc =
        |input: Vec<ProjectedMessage>| sigs(&add_empty_reasoning_content_markers(&input).unwrap());
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
        let route = model_error_target(&BoundarySnapshot::capture(&json(error)), &route()).unwrap();
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
