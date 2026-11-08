//! Shared pure overflow recovery; the gateway retains its host error envelope.
use super::engine::port::CompactionRecoveryPort;
use crate::host::{HostError, ModuleError};
pub use pilotdeck_context::recovery::ContextOverflowRecovery;
use pilotdeck_protocol::JsValue;
impl CompactionRecoveryPort for ContextOverflowRecovery {
    fn decide(&self, input: JsValue) -> Result<JsValue, HostError> {
        ContextOverflowRecovery::decide(self, input)
            .map_err(|error| ModuleError::new(error.message).into())
    }
}
pub fn fallback(input: &JsValue, first_keep_ratio: f64) -> Result<JsValue, HostError> {
    pilotdeck_context::recovery::fallback(input, first_keep_ratio)
        .map_err(|error| ModuleError::new(error.message).into())
}
