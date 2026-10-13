//! Pure first-party tool error advice. No effect, permission decision, clock,
//! retry or terminal publication is owned here. Source native graph properties
//! remain outside this immutable boundary interface.
use crate::ProjectionError;
use pilotdeck_protocol::{JsString, model::BoundarySnapshot};

#[derive(Debug, Clone)]
pub struct ToolErrorRecoveryInput {
    pub code: JsString,
    pub tool_name: JsString,
    pub message: JsString,
    pub cwd: JsString,
    pub permission_mode: JsString,
    pub details: Option<BoundarySnapshot>,
}

#[derive(Debug, Clone)]
pub struct ToolErrorRecoveryResult {
    pub message: JsString,
    pub advice: BoundarySnapshot,
}

/// Port of Original src/tool/execution/errorRecovery.ts for admitted immutable
/// values. Exact advice presence and UTF16 truncation belong to the pure policy;
/// the owning loop separately samples its clock before requesting this advice.
pub fn build_tool_error_recovery(
    input: &ToolErrorRecoveryInput,
) -> Result<ToolErrorRecoveryResult, ProjectionError> {
    crate::tool_error_recovery_policy::build(input)
}
