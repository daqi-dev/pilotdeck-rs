//! Shared fixed-baseline summary input and protected-anchor projections.
use super::*;
pub(super) use pilotdeck_context::summary::projection::{project_retained, project_summary};
pub(super) fn stable_anchors(messages: &[JsValue]) -> Result<Option<JsString>, HostError> {
    pilotdeck_context::summary::projection::stable_anchors(messages)
        .map_err(|e| failure(&e.message))
}
pub(super) fn protected_anchors(
    messages: &[JsValue],
    protected: &ProtectedNames,
) -> Result<Option<JsString>, HostError> {
    pilotdeck_context::summary::projection::protected_anchors(messages, protected)
        .map_err(|e| failure(&e.message))
}
