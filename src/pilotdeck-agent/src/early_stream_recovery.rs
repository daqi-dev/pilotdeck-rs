//! Pre-emission stream interruption and literal-unknown recovery only.
//! Effects and late fallible phases remain sequenced by the owning turn.
use crate::ports::PortResult;
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::BoundarySnapshot;

/// These counters are reset only when their respective Source gate is reached.
/// Output/repaired/empty recovery has separate state after these two gates.
#[derive(Debug, Default)]
pub struct EarlyStreamRecovery {
    pub(crate) interruption_count: u32,
    pub(crate) unknown_count: u32,
}

pub struct EarlyStreamInput<'a> {
    pub assembled: &'a BoundarySnapshot,
    pub assistant_message: &'a BoundarySnapshot,
    pub tool_calls: &'a [BoundarySnapshot],
    pub has_message_end: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyStreamKind {
    Interruption,
    UnknownFinish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyStreamAction {
    FallThrough,
    Retry {
        /// Only the interruption branch clears finalMessage for structured calls.
        clear_final_message: bool,
        /// Eligibility only: unknown retry still projects withoutThinkingBlocks
        /// before checking calls. Invoke partial_text_message for every retry.
        retain_partial_text: bool,
    },
    Exhausted,
}

/// Immutable input for later phases. Late projections follow the earlier
/// emission-port return. Native callback fences and stock-host consumption
/// fences belong to their declared delivery owners, not this pure policy.
#[derive(Debug)]
pub struct EarlyStreamPlan {
    pub action: EarlyStreamAction,
    pub kind: Option<EarlyStreamKind>,
    pub(crate) assembled: BoundarySnapshot,
    pub(crate) assistant_message: BoundarySnapshot,
    pub(crate) tool_calls: Vec<BoundarySnapshot>,
    pub(crate) structured_interruption: bool,
}

impl EarlyStreamRecovery {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn counters(&self) -> (u32, u32) {
        (self.interruption_count, self.unknown_count)
    }
    pub fn select(&mut self, input: EarlyStreamInput<'_>) -> PortResult<EarlyStreamPlan> {
        crate::early_stream_recovery_policy::select(self, input)
    }
}
impl EarlyStreamPlan {
    /// Retry: after any clear; exhausted: after terminal_error construction.
    /// Root appends/emits only when this returns Some. Direct native ports may
    /// await durability; Module sinks admit FIFO and the stock host consumer
    /// awaits its callback before dispatching later queued calls.
    pub fn partial_text_message(&self) -> PortResult<Option<BoundarySnapshot>> {
        crate::early_stream_recovery_policy::partial_text_message(self)
    }
    /// Retry only; evaluate after the partial emission port returns, then inject the
    /// prompt with Root UUID/metadata and publish Continued(model_error).
    pub fn retry_prompt(&self) -> PortResult<(JsString, JsString)> {
        crate::early_stream_recovery_policy::retry_prompt(self)
    }
    /// Exhausted only; Source constructs this BEFORE safe text selection.
    pub fn terminal_error(&self) -> PortResult<BoundarySnapshot> {
        crate::early_stream_recovery_policy::terminal_error(self)
    }
    /// Exhausted only; evaluate AFTER StopFailure and result creation.
    pub fn terminal_status(&self, error: &BoundarySnapshot) -> PortResult<BoundarySnapshot> {
        crate::early_stream_recovery_policy::terminal_status(self, error)
    }
}
