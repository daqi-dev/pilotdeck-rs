//! Pure original emergency terminal error and late status-detail projection.
//! b243 AgentLoop.ts 511–543,3515–3564,3622–3639,3876 and agentStatus.ts.
use crate::compaction_terminal::{
    EmergencyCompactionTerminalInput, EmergencyCompactionTerminalPlan,
};
use crate::ports::{PortError, PortResult};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

const ERROR_CODE: &str = "agent_context_recovery_failed";
const FALLBACK_CODE: &str = "context_overflow_after_emergency_compaction";
const EMERGENCY_HINT: &str = "The context is still too large after emergency compaction. Start a new session or reduce the request and retry.";
const STATUS_FALLBACK: &str = "The model request failed, so this turn has stopped.";
const EMPTY_MESSAGE: &str = "The model request failed.";
// No modelError was supplied by contextOverflowAfterEmergency. Its own hint
// stays on AgentError; the visible status uses modelFailureAction(undefined).
const SETTINGS_HINT: &str = "Check Settings → Model Provider and verify the selected provider, base URL, API key, model name, and timeoutMs. If the provider is slow or the network is unstable, increase timeoutMs or check provider status.";

fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn source_error(message: String, compact: &JsValue) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(compact),
    }
}
fn property(value: &JsValue, key: &str, compact: &JsValue) -> PortResult<JsValue> {
    if nullish(value) {
        return Err(source_error(
            format!(
                "Cannot read properties of {} (reading '{key}')",
                if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                }
            ),
            compact,
        ));
    }
    Ok(value.get(key).cloned().unwrap_or(JsValue::Undefined))
}
fn js_trim_space(unit: u16) -> bool {
    matches!(unit, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trimmed(value: &JsString) -> JsString {
    let units = value.units();
    let first = units
        .iter()
        .position(|unit| !js_trim_space(*unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !js_trim_space(*unit))
        .map_or(first, |at| at + 1);
    JsString::from_utf16(units[first..end].to_vec())
}
fn status_detail(message: &JsValue, compact: &JsValue) -> PortResult<BoundarySnapshot> {
    // Source uses message || modelError?.message || fallback; modelError absent.
    let provider_message = if truthy(message) {
        message.clone()
    } else {
        STATUS_FALLBACK.into()
    };
    let JsValue::String(provider_text) = &provider_message else {
        // This happens only AFTER StopFailure/result creation in the owner.
        // Preserve the known Source exception, not transport uncertainty.
        return Err(source_error(
            "providerMessage.trim is not a function".into(),
            compact,
        ));
    };
    let clean = trimmed(provider_text);
    let mut text = if clean.is_empty() {
        EMPTY_MESSAGE.encode_utf16().collect::<Vec<_>>()
    } else {
        clean.units().to_vec()
    };
    text.extend("\n\nAction: ".encode_utf16());
    text.extend(SETTINGS_HINT.encode_utf16());
    // createVisibleErrorStatusDetail -> createAgentStatusDetail -> pruneUndefined
    // removes only top-level undefined modelError fields. Nested params remain.
    let detail = object([
        ("message", JsValue::String(JsString::from_utf16(text))),
        (
            "messageI18n",
            object([
                ("key", "chat:agentStatus.modelRequestFailed.message".into()),
                (
                    "params",
                    object([("providerMessage", provider_message.clone())]),
                ),
            ]),
        ),
        ("code", ERROR_CODE.into()),
        ("severity", "error".into()),
        ("visible", JsValue::Bool(true)),
        ("userHint", SETTINGS_HINT.into()),
        (
            "userHintI18n",
            object([
                (
                    "key",
                    "chat:agentStatus.modelRequestFailed.actions.settingsDefault".into(),
                ),
                ("params", object([])),
            ]),
        ),
        ("scope", "turn".into()),
        ("source", "agent".into()),
        ("providerMessage", provider_message),
        ("fixTarget", "settings".into()),
    ]);
    Ok(BoundarySnapshot::capture(&detail))
}
pub fn evaluate(
    input: EmergencyCompactionTerminalInput<'_>,
) -> PortResult<EmergencyCompactionTerminalPlan> {
    let compact = input.compact.value();
    let code = property(compact, "error", compact)?;
    let message = if nullish(&code) {
        FALLBACK_CODE.into()
    } else {
        code.clone()
    };
    let snapshot = property(compact, "snapshot", compact)?;
    let result = property(compact, "result", compact)?;
    let diagnostics = if nullish(&result) {
        JsValue::Undefined
    } else {
        property(&result, "diagnostics", compact)?
    };
    let error = object([
        ("code", ERROR_CODE.into()),
        ("message", message.clone()),
        (
            "details",
            object([
                ("code", code),
                ("snapshot", snapshot),
                ("diagnostics", diagnostics),
            ]),
        ),
        ("userHint", EMERGENCY_HINT.into()),
    ]);
    let status_detail = status_detail(&message, compact);
    Ok(EmergencyCompactionTerminalPlan {
        error: BoundarySnapshot::capture(&error),
        status_detail,
    })
}
