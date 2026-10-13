//! Owning-worker invocation and bounded turn policy. This module does not yet
//! implement the full AgentLoop/TurnRunner or publish a durable turn completion.
use crate::ports::{
    CallControl, CancelCause, HostCall, HostPort, HostReply, ModelHints, PortError, PortResult,
    PreparedInvocation, Route, StreamEvent,
};
use crate::{
    AssembledAssistantMessage, StreamBlockKind, ToolExecutionResult, ToolResultDisposition,
    apply_model_event, assemble_assistant_message, create_model_assembler, stream_block_id,
};
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::{BoundarySnapshot, ModelEvent, Usage};

#[derive(Debug, Clone)]
pub struct LiveModelEvent {
    pub boundary: BoundarySnapshot,
    pub block_id: Option<JsString>,
}
#[derive(Debug, Clone)]
pub struct ExecutedRequest {
    pub route: Route,
    pub fingerprint: Option<JsString>,
}
#[derive(Debug, Clone)]
pub enum InvocationDisposition {
    Assembled,
    BoundaryError {
        error: BoundarySnapshot,
    },
    Cancelled(CancelCause),
    /// A lost advancing/effect reply remains AcceptedUnknown, never a fabricated
    /// provider content error eligible for a retry prompt.
    PortFailure(Box<PortError>),
}
#[derive(Debug, Clone)]
pub struct InvocationReport {
    pub assembled: Box<AssembledAssistantMessage>,
    pub executed: Option<ExecutedRequest>,
    pub disposition: InvocationDisposition,
}

/// Real port invocation. The caller provides a bounded, separate cleanup
/// control so an aborted main control does not prevent close_stream. The host
/// removes its preparation/iterator before awaiting return; close is best effort
/// and cannot replace a previously uncertain execution outcome.
pub fn run_model_invocation(
    host: &mut dyn HostPort,
    control: &CallControl,
    cleanup_control: &CallControl,
    prepared: &PreparedInvocation,
    hints: &ModelHints,
    response_id: JsString,
    emit: &mut dyn FnMut(LiveModelEvent) -> PortResult<()>,
) -> InvocationReport {
    let mut assembler = create_model_assembler(Some(response_id));
    let mut executed = None;
    let mut done = false;
    let mut stop_on_error_event = false;
    let mut disposition = InvocationDisposition::Assembled;
    let pull = host.advertised("model", "stream_next");
    loop {
        if let Err(error) = control.check() {
            disposition = interrupted(error);
            break;
        }
        let call = if pull {
            HostCall::ModelPull {
                handle: prepared.handle.clone(),
                request: Box::new(prepared.request.clone()),
                hints: hints.clone(),
            }
        } else {
            HostCall::ModelEagerStream {
                handle: prepared.handle.clone(),
                request: Some(Box::new(prepared.request.clone())),
                hints: hints.clone(),
            }
        };
        let reply = match host.call(control, call) {
            Ok(reply) => reply,
            Err(error) => {
                disposition = interrupted(error);
                break;
            }
        };
        let (events, reply_done) = match reply {
            HostReply::ModelChunk(chunk) if pull => (chunk.events, chunk.done),
            HostReply::ModelEagerEvents(events) if !pull => (events, true),
            _ => {
                disposition = interrupted(PortError::InvalidResponse {
                    message: "Model stream returned a reply for another operation.".into(),
                });
                break;
            }
        };
        for event in events {
            // A batch may already have been advanced. Cancellation prevents its
            // remaining events becoming executable tool calls.
            if let Err(error) = control.check() {
                disposition = interrupted(error);
                break;
            }
            if let StreamEvent::BoundaryOnly(boundary) = &event
                && matches!(
                    boundary.value(),
                    pilotdeck_protocol::JsValue::Null | pilotdeck_protocol::JsValue::Undefined
                )
            {
                // Original accesses event.type before yielding. Nullish values
                // throw there, rather than behaving like ignored unknown tags.
                let kind = if boundary.value().is_undefined() {
                    "undefined"
                } else {
                    "null"
                };
                disposition = interrupted(PortError::SourceException {
                    message: format!("Cannot read properties of {kind} (reading 'type')").into(),
                    boundary: boundary.clone(),
                });
                break;
            }
            let (boundary, block_id) = match &event {
                StreamEvent::Known(captured) => {
                    if let ModelEvent::RequestStarted {
                        provider,
                        model,
                        request_fingerprint,
                        ..
                    } = &captured.event
                    {
                        executed = Some(ExecutedRequest {
                            route: Route {
                                provider: provider.clone(),
                                model: model.clone(),
                            },
                            fingerprint: request_fingerprint.clone(),
                        });
                    }
                    let block_id = match captured.event {
                        ModelEvent::TextDelta { .. } => {
                            stream_block_id(&assembler, StreamBlockKind::Text)
                        }
                        ModelEvent::ThinkingDelta { .. } => {
                            stream_block_id(&assembler, StreamBlockKind::Thinking)
                        }
                        _ => None,
                    };
                    (captured.boundary.clone(), block_id)
                }
                StreamEvent::BoundaryOnly(boundary)
                | StreamEvent::DeferredSourceException { boundary, .. }
                | StreamEvent::ErrorBoundaryOnly { boundary, .. } => (boundary.clone(), None),
                StreamEvent::CoercedDelta { boundary, reducer } => {
                    let kind = match reducer.event {
                        ModelEvent::TextDelta { .. } => Some(StreamBlockKind::Text),
                        ModelEvent::ThinkingDelta { .. } => Some(StreamBlockKind::Thinking),
                        _ => None,
                    };
                    if kind.is_none() {
                        disposition = interrupted(PortError::InvalidResponse {
                            message: "Coerced event must contribute a text or thinking delta."
                                .into(),
                        });
                        break;
                    }
                    (
                        boundary.clone(),
                        stream_block_id(&assembler, kind.expect("checked delta kind")),
                    )
                }
            };
            // Source ordering: live event before assembler mutation. Unknown
            // event types are forwarded intact and ignored by the pure reducer.
            if let Err(error) = emit(LiveModelEvent { boundary, block_id }) {
                disposition = interrupted(error);
                break;
            }
            if let StreamEvent::DeferredSourceException { boundary, message } = &event {
                disposition = interrupted(PortError::SourceException {
                    message: message.clone(),
                    boundary: boundary.clone(),
                });
                break;
            }
            if let StreamEvent::ErrorBoundaryOnly { error, .. } = &event {
                // Source error flushes text/thinking before recording the full
                // error. No fabricated ModelError is used for a lax boundary.
                let _ = assemble_assistant_message(&mut assembler);
                assembler.error_boundary = Some(error.clone());
                assembler.finish_reason = Some(pilotdeck_protocol::model::FinishReason::Error);
                disposition = InvocationDisposition::BoundaryError {
                    error: error.clone(),
                };
                stop_on_error_event = true;
                break;
            }
            if let StreamEvent::Known(captured)
            | StreamEvent::CoercedDelta {
                reducer: captured, ..
            } = event
            {
                if let Err(error) = apply_model_event(&mut assembler, &captured) {
                    let crate::AssemblyError::InvalidBoundary { message } = error;
                    disposition = interrupted(PortError::InvalidResponse { message });
                    break;
                }
                if matches!(captured.event, ModelEvent::Error(..)) {
                    stop_on_error_event = true;
                    break;
                }
            }
        }
        // Source assigns response.done only after yielding the entire batch.
        // Early exit still closes even if the reply advertised done:true.
        if !stop_on_error_event && matches!(disposition, InvocationDisposition::Assembled) {
            done = reply_done;
        }
        if done || stop_on_error_event || !matches!(disposition, InvocationDisposition::Assembled) {
            break;
        }
    }
    if pull && !done && host.advertised("model", "close_stream") {
        let _ = host.call(
            cleanup_control,
            HostCall::ModelClose {
                handle: prepared.handle.clone(),
            },
        );
    }
    // A host callback can resolve while cancelling the turn. Cancellation is
    // checked after the final pull too, before any driver tool/recovery policy.
    if matches!(disposition, InvocationDisposition::Assembled)
        && let Err(error) = control.check()
    {
        disposition = interrupted(error);
    }
    InvocationReport {
        assembled: Box::new(assemble_assistant_message(&mut assembler)),
        executed,
        disposition,
    }
}
fn interrupted(error: PortError) -> InvocationDisposition {
    match error {
        PortError::Cancelled { cause } => InvocationDisposition::Cancelled(cause),
        error => InvocationDisposition::PortFailure(Box::new(error)),
    }
}
/// No-message_end, error, cancelled/uncertain or repaired output cannot authorize
/// partial tools. The full loop additionally handles finish/recovery policy.
pub fn invocation_can_execute_tools(report: &InvocationReport) -> bool {
    matches!(report.disposition, InvocationDisposition::Assembled)
        && report.assembled.has_message_end
        && report.assembled.error.is_none()
        && !(report.assembled.has_repaired_tool_calls == Some(true)
            && matches!(
                report.assembled.finish_reason,
                pilotdeck_protocol::model::FinishReason::Length
                    | pilotdeck_protocol::model::FinishReason::ToolCall
                    | pilotdeck_protocol::model::FinishReason::Stop
            ))
        && report.assembled.finish_reason != pilotdeck_protocol::model::FinishReason::Unknown
}
/// Every pre-route/routed/recovery/post-tool compaction path uses this barrier
/// before follow-up prepare/pull. Accepted uncertainty remains terminal/reconcile;
/// even a successful response cannot override cancellation by its owner.
pub fn persist_compaction_before_followup(
    host: &mut dyn HostPort,
    control: &CallControl,
    boundary: crate::ports::CompactBoundary,
    messages: Vec<crate::ProjectedMessage>,
) -> PortResult<crate::ports::CompactionCommit> {
    control.check()?;
    let commit = match host.call(
        control,
        HostCall::TurnPersistCompaction { boundary, messages },
    )? {
        HostReply::CompactionCommitted(commit) => commit,
        _ => {
            return Err(PortError::InvalidResponse {
                message: "Compaction response has an invalid operation.".into(),
            });
        }
    };
    control.check()?;
    Ok(commit)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetStop {
    Turn,
    Task,
}
#[derive(Debug, Clone)]
pub struct BudgetLedger {
    pub turn_limit_usd: Option<f64>,
    pub task_limit_usd: Option<f64>,
    pub initial_task_spent_usd: f64,
    pub task_spent_usd: f64,
    pub turn_spent_usd: f64,
}
impl BudgetLedger {
    pub fn new(turn: Option<f64>, task: Option<f64>, initial: f64) -> Self {
        Self {
            turn_limit_usd: turn,
            task_limit_usd: task,
            initial_task_spent_usd: initial,
            task_spent_usd: initial,
            turn_spent_usd: 0.0,
        }
    }
    /// Charge the actual executed route before tools, recovery or another model
    /// request. An unavailable host estimate may fall back to actual nativeCost;
    /// null and zero remain different. Host failures propagate, not free usage.
    pub fn charge(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        usage: Option<&Usage>,
        route: Route,
    ) -> PortResult<(Option<f64>, Option<BudgetStop>)> {
        let estimated = if host.advertised("budget", "estimate_usage_cost") {
            match host.call(
                control,
                HostCall::BudgetCost {
                    usage: usage.cloned(),
                    route,
                },
            )? {
                HostReply::BudgetCost(value) => value,
                _ => {
                    return Err(PortError::InvalidResponse {
                        message: "Budget cost reply has an invalid operation.".into(),
                    });
                }
            }
        } else {
            None
        };
        let cost = estimated
            .filter(|v| v.is_finite() && *v >= 0.0)
            .or_else(|| {
                usage
                    .and_then(|u| u.native_cost)
                    .filter(|v| v.is_finite() && *v >= 0.0)
            });
        Ok(self.apply_cost(cost))
    }
    /// Keep the exact Source-selected usage at the host cost boundary. The
    /// aggregate result separately projects only its known numeric fields.
    pub fn charge_source_usage(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        usage: Option<&BoundarySnapshot>,
        route: Route,
    ) -> PortResult<(Option<f64>, Option<BudgetStop>)> {
        let estimated = if host.advertised("budget", "estimate_usage_cost") {
            match host.call(
                control,
                HostCall::BudgetCostSnapshot {
                    usage: usage.cloned(),
                    route,
                },
            )? {
                HostReply::BudgetCost(value) => value,
                _ => {
                    return Err(PortError::InvalidResponse {
                        message: "Budget cost reply has an invalid operation.".into(),
                    });
                }
            }
        } else {
            None
        };
        let cost = estimated
            .filter(|v| v.is_finite() && *v >= 0.0)
            .or_else(|| {
                usage
                    .and_then(|u| u.value().get("nativeCost"))
                    .and_then(pilotdeck_protocol::JsValue::as_number)
                    .filter(|v| v.is_finite() && *v >= 0.0)
            });
        Ok(self.apply_cost(cost))
    }
    fn apply_cost(&mut self, cost: Option<f64>) -> (Option<f64>, Option<BudgetStop>) {
        if let Some(value) = cost {
            self.task_spent_usd += value;
            self.turn_spent_usd += value;
        }
        let turn = self
            .turn_limit_usd
            .is_some_and(|v| self.turn_spent_usd >= v);
        let task = self
            .task_limit_usd
            .is_some_and(|v| self.task_spent_usd >= v);
        // Source chooses the turn ceiling when both are reached.
        (
            cost,
            if turn {
                Some(BudgetStop::Turn)
            } else if task {
                Some(BudgetStop::Task)
            } else {
                None
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidBatchAction {
    Continue,
    GracePrompt,
    Stop,
}
#[derive(Debug, Clone, Default)]
pub struct RetryGuards {
    pub compact_attempted: bool,
    pub output_attempted: bool,
    pub empty_attempted: bool,
    pub reasoning_attempted: bool,
    pub image_strip_attempted: bool,
    pub output_continuations: u32,
    pub consecutive_empty: u32,
    pub json_corrections: u32,
    pub stream_interruptions: u32,
    pub unknown_finishes: u32,
    last_invalid_fingerprint: Option<JsString>,
    same_invalid_count: u32,
    invalid_grace_used: bool,
}
impl RetryGuards {
    pub fn retry_stream_interruption(&mut self) -> bool {
        increment_below(&mut self.stream_interruptions, 2)
    }
    pub fn retry_unknown_finish(&mut self) -> bool {
        increment_below(&mut self.unknown_finishes, 2)
    }
    pub fn retry_json(&mut self) -> bool {
        increment_below(&mut self.json_corrections, 3)
    }
    pub fn continue_output(&mut self) -> bool {
        increment_below(&mut self.output_continuations, 50)
    }
    pub fn observed_non_interrupted(&mut self) {
        self.stream_interruptions = 0;
    }
    /// Source reaches this reset whenever its exact unknown-finish recovery
    /// branch is not taken (including an error/missing end), after handling the
    /// stream-interruption branch. It is not conditional on a known finish.
    pub fn reset_unknown_finish_recovery(&mut self) {
        self.unknown_finishes = 0;
    }
    /// First empty attempt starts the output-continuation phase. Subsequent
    /// empties stop at the source's third consecutive count / fifty limit.
    pub fn retry_empty(&mut self) -> bool {
        if self.output_continuations > 0 {
            self.consecutive_empty += 1;
            self.consecutive_empty < 3 && self.continue_output()
        } else if !self.empty_attempted {
            self.empty_attempted = true;
            self.continue_output()
        } else {
            false
        }
    }
    /// Caller handles a pending LargeFileRepair first. Fingerprints use original
    /// UTF16 lexical sort and preserve duplicate name/error pairs.
    pub fn observe_tool_batch(&mut self, results: &[ToolExecutionResult]) -> InvalidBatchAction {
        let all_invalid = !results.is_empty() && results.iter().all(|r| matches!(
            &r.disposition, ToolResultDisposition::Error { code, .. } if code.equals_str("invalid_tool_input")
        ));
        if !all_invalid {
            self.last_invalid_fingerprint = None;
            self.same_invalid_count = 0;
            self.invalid_grace_used = false;
            self.output_continuations = 0;
            self.consecutive_empty = 0;
            self.output_attempted = false;
            self.empty_attempted = false;
            self.image_strip_attempted = false;
            // Source does NOT reset compact/reasoning/JSON turn guards here.
            return InvalidBatchAction::Continue;
        }
        let mut pairs: Vec<JsString> = results
            .iter()
            .filter_map(|r| {
                let ToolResultDisposition::Error { message, .. } = &r.disposition else {
                    return None;
                };
                let mut units = r.tool_name.units().to_vec();
                units.extend("::".encode_utf16());
                units.extend_from_slice(message.units());
                Some(JsString::from_utf16(units))
            })
            .collect();
        pairs.sort();
        let mut units = Vec::new();
        for (i, pair) in pairs.iter().enumerate() {
            if i > 0 {
                units.push(u16::from(b'\n'));
            }
            units.extend_from_slice(pair.units());
        }
        let fingerprint = JsString::from_utf16(units);
        if self.last_invalid_fingerprint.as_ref() == Some(&fingerprint) {
            self.same_invalid_count += 1;
        } else {
            self.last_invalid_fingerprint = Some(fingerprint);
            self.same_invalid_count = 1;
            self.invalid_grace_used = false;
        }
        if self.same_invalid_count < 3 {
            InvalidBatchAction::Continue
        } else if !self.invalid_grace_used {
            self.invalid_grace_used = true;
            InvalidBatchAction::GracePrompt
        } else {
            InvalidBatchAction::Stop
        }
    }
}
fn increment_below(counter: &mut u32, limit: u32) -> bool {
    if *counter >= limit {
        false
    } else {
        *counter += 1;
        true
    }
}
