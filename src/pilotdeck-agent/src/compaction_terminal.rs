//! Source emergency compaction error/status projection, without port effects.
use crate::ports::PortResult;
use pilotdeck_protocol::model::BoundarySnapshot;

pub struct EmergencyCompactionTerminalInput<'a> {
    pub compact: &'a BoundarySnapshot,
}
pub struct EmergencyCompactionTerminalPlan {
    pub error: BoundarySnapshot,
    /// Source emits the status after StopFailure dispatch and result creation.
    /// Keep malformed-message failures in that later phase.
    pub status_detail: PortResult<BoundarySnapshot>,
}
pub struct OriginalEmergencyCompactionTerminal;
impl OriginalEmergencyCompactionTerminal {
    pub fn evaluate(
        &self,
        input: EmergencyCompactionTerminalInput<'_>,
    ) -> PortResult<EmergencyCompactionTerminalPlan> {
        crate::compaction_terminal_policy::evaluate(input)
    }
}
