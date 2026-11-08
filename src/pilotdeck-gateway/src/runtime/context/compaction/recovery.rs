//! Shared pure overflow recovery; the gateway retains its host error envelope.
use super::engine::port::CompactionRecoveryPort;
use crate::host::{HostError, ModuleError};
#[derive(Clone, Debug)]
pub struct ContextOverflowRecovery {
    pub truncate_first_keep_ratio: f64,
    pub truncate_second_keep_ratio: f64,
}
impl Default for ContextOverflowRecovery {
    fn default() -> Self {
        let defaults = pilotdeck_context::recovery::ContextOverflowRecovery::default();
        Self {
            truncate_first_keep_ratio: defaults.truncate_first_keep_ratio,
            truncate_second_keep_ratio: defaults.truncate_second_keep_ratio,
        }
    }
}
impl ContextOverflowRecovery {
    pub fn decide(&self, input: JsValue) -> Result<JsValue, HostError> {
        pilotdeck_context::recovery::ContextOverflowRecovery {
            truncate_first_keep_ratio: self.truncate_first_keep_ratio,
            truncate_second_keep_ratio: self.truncate_second_keep_ratio,
        }
        .decide(input)
        .map_err(|error| ModuleError::new(error.message).into())
    }
}
use pilotdeck_protocol::JsValue;
impl CompactionRecoveryPort for ContextOverflowRecovery {
    fn decide(&self, input: JsValue) -> Result<JsValue, HostError> {
        ContextOverflowRecovery::decide(self, input)
    }
}
pub fn fallback(input: &JsValue, first_keep_ratio: f64) -> Result<JsValue, HostError> {
    pilotdeck_context::recovery::fallback(input, first_keep_ratio)
        .map_err(|error| ModuleError::new(error.message).into())
}
