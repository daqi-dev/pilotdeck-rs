//! Source ordered repeated-failure annotations and invalid-input guard.
//! Pure turn-local state; Root owns pairing, LFR, events and all effects.
use crate::ports::PortResult;
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::BoundarySnapshot;

#[derive(Debug, Default)]
pub struct ToolFailureRecovery {
    pub(crate) last_tool_failure_fingerprint: Option<JsString>,
    pub(crate) last_invalid_fingerprint: Option<JsString>,
    pub(crate) same_invalid_count: u32,
    pub(crate) used_invalid_grace: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidToolAction {
    FallThrough { reset_output_recovery: bool },
    Continue { prompt: JsString, purpose: JsString },
    Stop { repeated_failures: u32 },
}
#[derive(Debug, Clone)]
pub struct InvalidToolPlan {
    pub action: InvalidToolAction,
}
impl ToolFailureRecovery {
    pub fn new() -> Self {
        Self::default()
    }
    /// After FIFO pairing, before LFR and result publication. Commit the new
    /// fingerprint only after the entire Source annotation succeeds.
    pub fn annotate_results(
        &mut self,
        results: &[BoundarySnapshot],
    ) -> PortResult<Vec<BoundarySnapshot>> {
        crate::tool_failures_policy::annotate_results(self, results)
    }
    /// A non-mutating query lets Root run pending LFR before circuit counting.
    pub fn all_invalid(results: &[BoundarySnapshot]) -> PortResult<bool> {
        crate::tool_failures_policy::all_invalid(results)
    }
    /// After LFR and lifecycle-block gates decline and all projected messages
    /// are durable. FallThrough's reset bit refers to the separate output owner.
    pub fn select_invalid(&mut self, results: &[BoundarySnapshot]) -> PortResult<InvalidToolPlan> {
        crate::tool_failures_policy::select_invalid(self, results)
    }
    pub fn counters(&self) -> (Option<&JsString>, Option<&JsString>, u32, bool) {
        (
            self.last_tool_failure_fingerprint.as_ref(),
            self.last_invalid_fingerprint.as_ref(),
            self.same_invalid_count,
            self.used_invalid_grace,
        )
    }
}
impl InvalidToolPlan {
    /// Source constructs the error/result before the late status callback.
    pub fn terminal_error(&self) -> PortResult<BoundarySnapshot> {
        crate::tool_failures_policy::terminal_error(self)
    }
    pub fn terminal_status(&self, error: &BoundarySnapshot) -> PortResult<BoundarySnapshot> {
        crate::tool_failures_policy::terminal_status(self, error)
    }
}
