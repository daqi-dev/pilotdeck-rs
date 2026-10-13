//! Bounded port of Original AgentLoop.ts1402–1441 and createMissingToolResult.
//! The fixed interruption message makes all errorRecovery.ts branches concrete:
//! one evidence line, environment_issue, default permission-mode advice, and no
//! avoidRetryReason. Native getters/callables/symbols are not BoundarySnapshot
//! values; their observable behavior belongs to the host binding gate.
use crate::ProjectionError;
use crate::ordinary_error_recovery::{JsonSelfCorrectionPlan, ModelInterruptedToolResultInput};
use pilotdeck_protocol::{JsObject, JsString, JsValue, model::BoundarySnapshot, stringify_json};

const MESSAGE: &str = "Model error interrupted tool execution.";
const PROMPT: &str = "Your previous tool call contained invalid JSON in the arguments and could not be parsed. Please retry with valid JSON. Common issues: missing quotes around keys/values, trailing commas, unescaped special characters in strings.";

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert(key.into(), value);
    }
    JsValue::Object(result)
}

fn property(value: &JsValue, key: &str) -> Result<JsValue, ProjectionError> {
    match value {
        JsValue::Null | JsValue::Undefined => Err(ProjectionError::SourceTypeError {
            message: format!(
                "Cannot read properties of {} (reading '{key}')",
                if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                }
            )
            .as_str()
            .into(),
        }),
        // These keys (name/id/code) have no inherited primitive/array property
        // in the native-free Original realm. Preserve absent as own undefined
        // when constructing the result instead of inventing an identifier.
        _ => Ok(value.get(key).cloned().unwrap_or(JsValue::Undefined)),
    }
}

fn concat(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|p| p.units().iter().copied())
            .collect(),
    )
}

/// Template-literal ToString for immutable JSON+Undefined values. Iterative
/// array joining preserves lone UTF16 and avoids recursive native stack use.
/// An own noncallable toString masks the inherited method; valueOf on this
/// native-free object cannot produce a primitive, so Original throws TypeError.
fn template(value: &JsValue) -> Result<JsString, ProjectionError> {
    enum Piece<'a> {
        Value(&'a JsValue),
        Comma,
    }
    let mut stack = vec![Piece::Value(value)];
    let mut units = Vec::new();
    while let Some(piece) = stack.pop() {
        let value = match piece {
            Piece::Comma => {
                units.push(0x2c);
                continue;
            }
            Piece::Value(value) => value,
        };
        let text: JsString = match value {
            JsValue::Undefined => "undefined".into(),
            JsValue::Null => "null".into(),
            JsValue::Bool(true) => "true".into(),
            JsValue::Bool(false) => "false".into(),
            JsValue::String(text) => text.clone(),
            JsValue::Number(n) if n.is_nan() => "NaN".into(),
            JsValue::Number(n) if *n == f64::INFINITY => "Infinity".into(),
            JsValue::Number(n) if *n == f64::NEG_INFINITY => "-Infinity".into(),
            JsValue::Number(_) => stringify_json(value)
                .map_err(|error| ProjectionError::Codec {
                    message: error.to_string().as_str().into(),
                })?
                .ok_or_else(|| ProjectionError::Codec {
                    message: "Finite number stringify returned undefined".into(),
                })?
                .as_str()
                .into(),
            JsValue::Object(fields) => {
                if fields.contains_key("toString") {
                    return Err(ProjectionError::SourceTypeError {
                        message: "Cannot convert object to primitive value".into(),
                    });
                }
                "[object Object]".into()
            }
            JsValue::Array(values) => {
                for (index, item) in values.iter().enumerate().rev() {
                    if !matches!(item, JsValue::Null | JsValue::Undefined) {
                        stack.push(Piece::Value(item));
                    }
                    if index != 0 {
                        stack.push(Piece::Comma);
                    }
                }
                continue;
            }
        };
        units.extend_from_slice(text.units());
    }
    Ok(JsString::from_utf16(units))
}

pub(crate) fn missing_result(
    input: ModelInterruptedToolResultInput<'_>,
) -> Result<BoundarySnapshot, ProjectionError> {
    // Original samples timestamp first (already owned by caller), reads name
    // for advice, then reads id/name when returning the result. No strict call
    // admission: missing, null and numeric IDs/names retain their raw values.
    let name = property(input.call.value(), "name")?;
    let rendered_name = template(&name)?;
    let action = concat(&[
        &"Inspect the evidence, change the approach, then retry only with corrected inputs. Current permission mode: ".into(),
        input.permission_mode,
        &".".into(),
    ]);
    let advice = object([
        ("summary", MESSAGE.into()),
        ("failureClass", "environment_issue".into()),
        (
            "nextActions",
            JsValue::Array(vec![JsValue::String(action.clone())]),
        ),
        ("salientEvidence", JsValue::Array(vec![MESSAGE.into()])),
        ("originalError", MESSAGE.into()),
    ]);
    let text = concat(&[
        &"TOOL_ERROR[tool_execution_failed][".into(), &rendered_name,
        &"][environment_issue]\nSummary: Model error interrupted tool execution.\nEvidence:\n- Model error interrupted tool execution.\nOriginal error:\nModel error interrupted tool execution.\nNext actions:\n1. ".into(),
        &action,
    ]);
    Ok(BoundarySnapshot::capture(&object([
        ("type", "error".into()),
        ("toolCallId", property(input.call.value(), "id")?),
        ("toolName", name),
        (
            "error",
            object([
                ("code", "tool_execution_failed".into()),
                ("message", MESSAGE.into()),
            ]),
        ),
        (
            "content",
            JsValue::Array(vec![object([
                ("type", "text".into()),
                ("text", JsValue::String(text)),
            ])]),
        ),
        ("metadata", object([("recovery", advice)])),
        ("startedAt", JsValue::String(input.timestamp.clone())),
        ("completedAt", JsValue::String(input.timestamp.clone())),
    ])))
}

pub(crate) fn json_plan(
    error: &BoundarySnapshot,
    enabled: bool,
    attempts_used: u32,
) -> Result<JsonSelfCorrectionPlan, ProjectionError> {
    // Original && order: disabled avoids the property read; enabled reads code
    // even if the retry counter has already reached three. No coercion of code.
    if enabled
        && matches!(property(error.value(), "code")?, JsValue::String(ref code) if code.equals_str("invalid_tool_arguments"))
        && attempts_used < 3
    {
        return Ok(JsonSelfCorrectionPlan::Continue {
            attempts_used: attempts_used + 1,
            prompt: PROMPT.into(),
            purpose: "json_self_correct".into(),
        });
    }
    Ok(JsonSelfCorrectionPlan::Decline { attempts_used })
}
