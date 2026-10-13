//! Pure budget terminal projection after the owning loop charges actual usage.
//! This policy cannot call a model, tool, permission, lifecycle or persistence port.
use crate::ProjectedMessage;
use crate::driver::{BudgetLedger, BudgetStop};
use crate::ports::{PortResult, Route};
use crate::turn::TurnStopReason;
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::BoundarySnapshot;

pub struct BudgetTerminalInput<'a> {
    pub ledger: &'a BudgetLedger,
    pub stop: BudgetStop,
    pub last_invocation_cost_usd: Option<f64>,
    pub route: &'a Route,
    pub assistant_message: &'a ProjectedMessage,
    pub unexecuted_tool_call_count: usize,
}

#[derive(Debug, Clone)]
pub struct BudgetTerminalPlan {
    pub error: BoundarySnapshot,
    pub status_event: JsString,
    /// Source AgentStatus public detail is the error details object itself.
    pub status_detail: BoundarySnapshot,
    pub safe_message: Option<ProjectedMessage>,
    pub stop_reason: TurnStopReason,
}

pub struct OriginalBudgetTerminalPolicy;
impl OriginalBudgetTerminalPolicy {
    pub fn evaluate(&self, input: BudgetTerminalInput<'_>) -> PortResult<BudgetTerminalPlan> {
        crate::budget_terminal_policy::evaluate(input)
    }
}
