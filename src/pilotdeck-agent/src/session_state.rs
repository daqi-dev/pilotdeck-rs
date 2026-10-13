//! Explicit volatile AgentLoop projections. Session remains the durable owner.
use crate::ports::{PortResult, Route, TokenCalibration};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsString, JsValue};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    Text,
    Image,
    Pdf,
    Notebook,
}
#[derive(Debug, Clone)]
pub struct ReadFileEntry {
    pub mtime_ms: f64,
    pub kind: ReadKind,
    pub offset: Option<f64>,
    pub limit: Option<f64>,
    pub pages: Option<JsString>,
}
#[derive(Debug, Clone)]
pub struct WriteSnapshotEntry {
    pub absolute_path: JsString,
    pub mtime_ms: f64,
    pub content_hash: JsString,
    pub offset: Option<f64>,
    pub limit: Option<f64>,
}
#[derive(Debug, Clone, Default)]
pub struct AgentFileState {
    /// Map insertion order is part of the original serialized projection.
    pub read_file_state: Vec<(JsString, ReadFileEntry)>,
    pub write_snapshots: Vec<(JsString, WriteSnapshotEntry)>,
    pub allowed_read_files: Vec<JsString>,
}
#[derive(Debug, Clone)]
pub struct PersistentTokenCap {
    pub route: Route,
    pub max_context_tokens: Option<f64>,
    pub session_max_output_tokens: Option<f64>,
    pub hard_max_output_tokens: Option<f64>,
}
#[derive(Debug, Clone, Default)]
pub struct AgentModelSessionState {
    pub token_calibration: Vec<TokenCalibration>,
    pub token_caps: Vec<PersistentTokenCap>,
}
/// Source-equivalent validation; JSON own-value profile is separate from native
/// Map/prototype/alias graphs. No file is opened and no supplied path is authority.
pub fn parse_file_state(value: &JsValue) -> PortResult<Option<AgentFileState>> {
    crate::session_state_projection::parse_file(value)
}
pub fn parse_model_state(value: &JsValue) -> PortResult<Option<AgentModelSessionState>> {
    crate::session_state_projection::parse_model(value)
}
pub fn snapshot_file_state(state: &AgentFileState) -> BoundarySnapshot {
    crate::session_state_projection::snapshot_file(state)
}
pub fn snapshot_model_state(state: &AgentModelSessionState) -> BoundarySnapshot {
    crate::session_state_projection::snapshot_model(state)
}

/// Original recordTokenCalibration: positive finite reported input/cache fields
/// sum in Source order, ceil once, and update the joined UTF-16 Map key in place.
/// Output-only/fallback estimates never replace the reported input observation.
pub fn record_token_calibration(
    state: &mut AgentModelSessionState,
    route: &Route,
    usage: Option<&BoundarySnapshot>,
    estimated_input_tokens: Option<f64>,
) {
    let Some(estimate) = estimated_input_tokens else {
        return;
    };
    if estimate <= 0.0 {
        return;
    }
    let Some(usage) = usage else {
        return;
    };
    let mut total = 0.0;
    for field in ["inputTokens", "cacheReadTokens", "cacheWriteTokens"] {
        if let Some(tokens) = usage.value().get(field).and_then(JsValue::as_number)
            && tokens.is_finite()
            && tokens > 0.0
        {
            total += tokens;
        }
    }
    if total <= 0.0 {
        return;
    }
    let calibration = TokenCalibration {
        route: route.clone(),
        actual_input_tokens: total.ceil(),
        estimated_input_tokens: estimate,
    };
    let key = |r: &Route| {
        r.provider
            .units()
            .iter()
            .copied()
            .chain(std::iter::once(0))
            .chain(r.model.units().iter().copied())
            .collect::<Vec<u16>>()
    };
    let requested = key(route);
    if let Some(previous) = state
        .token_calibration
        .iter_mut()
        .find(|c| key(&c.route) == requested)
    {
        *previous = calibration;
    } else {
        state.token_calibration.push(calibration);
    }
}
