//! Shared invocation outcomes and bounded turn policy helpers.
//! Source model invocation and the owning turn consume these types and policies.
use crate::ports::{
    CallControl, CancelCause, HostCall, HostPort, HostReply, PortError, PortResult, Route,
};
use crate::{ToolExecutionResult, ToolResultDisposition};
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::{BoundarySnapshot, Usage};

#[derive(Debug, Clone)]
pub struct LiveModelEvent {
    pub boundary: BoundarySnapshot,
    pub block_id: Option<JsString>,
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
