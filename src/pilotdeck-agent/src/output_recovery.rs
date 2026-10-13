//! Repaired-call and empty-output gates AFTER early stream recovery.
//! Four closed counters/flags; LargeFileRepair is a separate owner.
use crate::LargeFileRepairDecision;
use crate::ports::{PortResult, Route};
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::BoundarySnapshot;

#[derive(Debug, Default)]
pub struct OutputRecovery {
    pub(crate) attempted_output_retry: bool,
    pub(crate) attempted_empty_retry: bool,
    pub(crate) output_recovery_count: u32,
    pub(crate) consecutive_empty_count: u32,
}
#[derive(Debug, Clone, Copy)]
pub struct OutputRecoveryInput<'a> {
    pub assembled: &'a BoundarySnapshot,
    pub assistant_message: &'a BoundarySnapshot,
    pub tool_calls: &'a [BoundarySnapshot],
    pub has_repaired_tool_calls: bool,
    pub original_request: &'a BoundarySnapshot,
    pub routed_route: &'a Route,
    /// Immutable pure currentMaxOutputTokens projection, not a Host invocation.
    pub current_max_output_tokens: Option<f64>,
    pub routed_model_max_output_tokens: Option<f64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryCapEvent {
    TokenCapAdjusted,
    EmptyOutputRecovery,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecoveryCapChange {
    pub event: RecoveryCapEvent,
    pub previous: Option<f64>,
    pub next: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub enum OutputRecoveryAction {
    FallThrough,
    Continue {
        cap_change: Option<RecoveryCapChange>,
        /// A token bump has no synthetic prompt. Both branches publish Continued.
        prompt: Option<(JsString, JsString)>,
    },
    UseLargeFileDecision {
        decision: LargeFileRepairDecision,
        /// Source repaired gate has not yet appended this dirty assistant.
        strip_current_assistant: bool,
    },
    /// Residual max-output Phase A: remove the durable error pair before cap change.
    StripErrorPairThenContinue {
        cap_change: RecoveryCapChange,
    },
    /// Visible text Phase C: a deferred warning, followed by ordinary completion.
    VisibleLengthExhausted,
    RepairedExhausted,
    EmptyExhausted {
        attempts: u32,
    },
}
#[derive(Debug)]
pub struct OutputRecoveryPlan {
    pub action: OutputRecoveryAction,
    pub(crate) original_request: BoundarySnapshot,
    pub(crate) routed_route: Route,
    pub(crate) assembled: BoundarySnapshot,
    pub(crate) output_recovery_count: u32,
}
#[derive(Debug)]
pub enum OutputRecoverySelection<'a> {
    Plan(OutputRecoveryPlan),
    NeedsLargeFileRepair(RepairedGateContinuation<'a>),
}
/// Affine token borrows the exact originating state. No policy counter changes
/// until Root performs the single selected name-only LargeFileRepair query.
#[derive(Debug)]
pub struct RepairedGateContinuation<'a> {
    pub(crate) state: &'a mut OutputRecovery,
    pub(crate) input: OutputRecoveryInput<'a>,
}
impl OutputRecovery {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn counters(&self) -> (bool, bool, u32, u32) {
        (
            self.attempted_output_retry,
            self.attempted_empty_retry,
            self.output_recovery_count,
            self.consecutive_empty_count,
        )
    }
    /// Called only after earlier durable model-error recovery gates decline.
    pub fn select_residual_max_output(
        &mut self,
        input: OutputRecoveryInput<'_>,
    ) -> PortResult<OutputRecoveryPlan> {
        crate::output_recovery_policy::select_residual_max_output(self, input)
    }
    /// Called after a durable nonempty, no-tool, non-error assistant with length.
    pub fn select_visible_length(
        &mut self,
        input: OutputRecoveryInput<'_>,
    ) -> PortResult<OutputRecoveryPlan> {
        crate::output_recovery_policy::select_visible_length(self, input)
    }
    /// Source commits the same attempt flag BEFORE fallible reactive cap lookup.
    pub fn consume_reactive_output_attempt(&mut self) -> bool {
        if self.attempted_output_retry {
            false
        } else {
            self.attempted_output_retry = true;
            true
        }
    }
    pub fn select<'a>(
        &'a mut self,
        input: OutputRecoveryInput<'a>,
    ) -> PortResult<OutputRecoverySelection<'a>> {
        crate::output_recovery_policy::select(self, input)
    }
}
impl RepairedGateContinuation<'_> {
    pub fn resolve(
        self,
        decision: Option<LargeFileRepairDecision>,
    ) -> PortResult<OutputRecoveryPlan> {
        crate::output_recovery_policy::resolve(self, decision)
    }
}
impl OutputRecoveryPlan {
    /// Routed cap event labels, distinct from original-request status labels.
    pub fn routed_route(&self) -> &Route {
        &self.routed_route
    }
    /// Source finishReason for the deferred empty_output_recovery event.
    pub fn assembled(&self) -> &BoundarySnapshot {
        &self.assembled
    }

    /// Repaired exhaustion only; before Source result construction/status.
    pub fn terminal_error(&self) -> PortResult<BoundarySnapshot> {
        crate::output_recovery_policy::terminal_error(self)
    }
    /// Repaired: after result construction. Empty: after prior assistant
    /// selection and BEFORE success/completed result construction.
    pub fn terminal_status(
        &self,
        error: Option<&BoundarySnapshot>,
    ) -> PortResult<BoundarySnapshot> {
        crate::output_recovery_policy::terminal_status(self, error)
    }
}
