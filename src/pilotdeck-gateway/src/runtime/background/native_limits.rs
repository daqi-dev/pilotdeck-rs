//! Native CLI resource bounds. Legacy TS-parity runtime construction keeps its
//! existing defaults; only the explicit native composition selects this policy.
use super::{BackgroundTaskError, StartTaskSpec};
use pilotdeck_protocol::JsValue;

#[derive(Clone, Copy)]
pub(crate) struct NativeTaskLimits {
    pub retained_tasks: usize,
    pub metadata_units: usize,
    pub output_bytes: usize,
    pub read_bytes: usize,
}
impl NativeTaskLimits {
    pub const CLI: Self = Self {
        retained_tasks: 256,
        metadata_units: 4096,
        output_bytes: 16 * 1024 * 1024,
        read_bytes: 512 * 1024,
    };
    pub fn validate_spec(self, spec: &StartTaskSpec) -> Result<(), BackgroundTaskError> {
        if let JsValue::String(kind) = &spec.kind
            && !kind.equals_str("bash")
            && !kind.equals_str("monitor")
        {
            return Err(BackgroundTaskError::error(
                "Native background task kind must be bash or monitor.".into(),
            ));
        }
        let mut units = spec.cwd.units().len();
        if matches!(&spec.kind, JsValue::Null | JsValue::Undefined) {
            units = units.saturating_add(4);
        }
        for value in [&spec.command, &spec.session_id, &spec.agent_id, &spec.kind] {
            match value {
                JsValue::String(value) => units = units.saturating_add(value.units().len()),
                JsValue::Null | JsValue::Undefined => {}
                _ => {
                    return Err(BackgroundTaskError::error(
                        "Native background task metadata must contain strings.".into(),
                    ));
                }
            }
        }
        if !matches!(&spec.command, JsValue::String(_)) || units > self.metadata_units {
            return Err(BackgroundTaskError::error(format!(
                "Native background task metadata limit ({} UTF-16 units) exceeded; no process was started.", self.metadata_units).into()));
        }
        Ok(())
    }
}
