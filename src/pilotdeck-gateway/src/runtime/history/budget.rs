use super::project::ProjectionError;
use pilotdeck_protocol::{JsValue, stringify_json};
/// Counts serialized message bytes before they enter the accumulated result.
/// This protects the WS frame budget, not total process RSS.
#[derive(Default)]
pub(super) struct Budget {
    bytes: usize,
}
impl Budget {
    pub fn reserve(&mut self, bytes: usize) -> Result<(), ProjectionError> {
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(|| ProjectionError("History output exceeds Gateway frame limit".into()))?;
        if self.bytes > crate::native_ws::MAX_OUTPUT_PAYLOAD { return Err(ProjectionError("History output exceeds Gateway frame limit".into())); }
        Ok(())
    }
    pub fn charge(&mut self, value: &JsValue) -> Result<(), ProjectionError> {
        let len = stringify_json(value)
            .map_err(|e| ProjectionError(e.to_string()))?
            .map_or(0, |s| s.len());
        self.bytes = self
            .bytes
            .checked_add(len.saturating_add(1))
            .ok_or_else(|| ProjectionError("History output exceeds Gateway frame limit".into()))?;
        if self.bytes > crate::native_ws::MAX_OUTPUT_PAYLOAD {
            return Err(ProjectionError(
                "History output exceeds Gateway frame limit".into(),
            ));
        }
        Ok(())
    }
}
