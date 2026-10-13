//! Pure proposals for the Original ordinary-model-error branch.
//! The owning turn first publishes and durably accepts its assistant, declines
//! the earlier one-shot reasoning retry, and then applies these proposals in
//! source order. This module performs no IO, clock/UUID sampling, tool dispatch,
//! permission decision, durable acknowledgement, or model retry itself.
//! AcceptedUnknown is retained by the owner; it cannot invoke a later effect
//! until the accepted operation's physical receipt is resolved.
use crate::ProjectionError;
use pilotdeck_protocol::{JsString, model::BoundarySnapshot};

pub struct ModelInterruptedToolResultInput<'a> {
    /// Complete immutable Original tool-call value; IDs are never invented.
    pub call: &'a BoundarySnapshot,
    /// Already sampled once for this call by its owning clock.
    pub timestamp: &'a JsString,
    pub cwd: &'a JsString,
    pub permission_mode: &'a JsString,
}

/// Return the complete createMissingToolResult object, including advice,
/// own-field presence, content, error and both identical sampled timestamps.
/// Ordered calls use ordered invocations; the caller subsequently uses the
/// canonical projection and durable message fence. No call is executed here.
pub fn create_model_interrupted_tool_result(
    input: ModelInterruptedToolResultInput<'_>,
) -> Result<BoundarySnapshot, ProjectionError> {
    crate::ordinary_error_recovery_policy::missing_result(input)
}

#[derive(Debug, Clone)]
pub enum JsonSelfCorrectionPlan {
    Decline {
        attempts_used: u32,
    },
    Continue {
        attempts_used: u32,
        prompt: JsString,
        purpose: JsString,
    },
}

/// Source strict code equality and at most three turn-owned JSON corrections.
/// Tool-result pairing/durability precedes the transient prompt; its real
/// transient ID, message insertion, consumed expiry and turn_continued event
/// belong to the owning turn. Count is not a general model-retry budget.
pub fn plan_json_self_correction(
    error: &BoundarySnapshot,
    enabled: bool,
    attempts_used: u32,
) -> Result<JsonSelfCorrectionPlan, ProjectionError> {
    crate::ordinary_error_recovery_policy::json_plan(error, enabled, attempts_used)
}
