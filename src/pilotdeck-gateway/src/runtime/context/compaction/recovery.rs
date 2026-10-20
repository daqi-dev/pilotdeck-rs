//! Provider overflow decisions. The caller owns retry count and model calls.
use super::{engine::port::CompactionRecoveryPort, *};
use crate::host::{HostError, ModuleError};

#[derive(Clone, Debug)]
pub struct ContextOverflowRecovery {
    pub truncate_first_keep_ratio: f64,
    pub truncate_second_keep_ratio: f64,
}
impl Default for ContextOverflowRecovery {
    fn default() -> Self {
        Self {
            truncate_first_keep_ratio: 0.5,
            truncate_second_keep_ratio: 0.25,
        }
    }
}
impl CompactionRecoveryPort for ContextOverflowRecovery {
    fn decide(&self, input: JsValue) -> Result<JsValue, HostError> {
        let error = error_field(&input)?;
        let attempted = js_truthy(field(&input, "hasAttemptedCompact"));
        if let Some(result) = image_recovery(error) {
            return Ok(result);
        }
        let available = number(field(error, "availableOutputTokens"))?;
        if available > 0.0 {
            let available = available.floor();
            if available < 4096.0 {
                if attempted {
                    return Ok(truncate(
                        self.truncate_second_keep_ratio,
                        "provider-available-output-too-small-after-compact",
                    ));
                }
                let mut fields = vec![("type", "compact_and_retry".into())];
                let context = number(field(error, "maxContextTokens"))?;
                if context > 0.0 {
                    fields.push(("maxContextTokens", JsValue::Number(context.floor())));
                }
                fields.extend([
                    ("maxOutputTokens", JsValue::Number(4096.0)),
                    ("reason", "provider-available-output-too-small".into()),
                ]);
                return Ok(object(fields));
            }
            return Ok(adjust(available, "attempt"));
        }
        let output = number(field(error, "maxOutputTokens"))?;
        if output > 0.0 {
            return Ok(adjust(output.floor(), "hard_cap"));
        }
        if !is_context_error(error) {
            return give_up(error);
        }
        if attempted {
            return Ok(truncate(
                self.truncate_second_keep_ratio,
                "ptl-second-attempt",
            ));
        }
        let context = number(field(error, "maxContextTokens"))?;
        if context > 0.0 {
            return Ok(object([
                ("type", "compact_and_retry".into()),
                ("maxContextTokens", JsValue::Number(context.floor())),
                ("reason", "provider-context-cap".into()),
            ]));
        }
        Ok(truncate(
            self.truncate_first_keep_ratio,
            "ptl-first-attempt",
        ))
    }
}

/// DefaultContextRuntime deliberately has a different legacy fallback:
/// without a recovery port it gives up after the first truncation.
pub fn fallback(input: &JsValue, first_keep_ratio: f64) -> Result<JsValue, HostError> {
    let error = error_field(input)?;
    if let Some(result) = image_recovery(error) {
        return Ok(result);
    }
    if !is_context_error(error) {
        return give_up(error);
    }
    if js_truthy(field(input, "hasAttemptedCompact")) {
        return Ok(object([
            ("type", "give_up".into()),
            ("reason", "ptl-exhausted-after-two-attempts".into()),
        ]));
    }
    Ok(truncate(first_keep_ratio, "ptl-first-attempt"))
}
fn number(value: &JsValue) -> Result<f64, HostError> {
    crate::js_value::to_number(value)
        .map_err(|_| ModuleError::new(crate::js_value::StackOverflow::MESSAGE).into())
}
fn error_field(input: &JsValue) -> Result<&JsValue, HostError> {
    for (receiver, name) in [
        (input, "error"),
        (field(input, "error"), "recoverableViaImageStrip"),
    ] {
        if matches!(receiver, JsValue::Undefined | JsValue::Null) {
            let kind = if receiver.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            return Err(ModuleError::new(format!(
                "Cannot read properties of {kind} (reading '{name}')"
            ))
            .into());
        }
    }
    Ok(field(input, "error"))
}
fn image_recovery(error: &JsValue) -> Option<JsValue> {
    let reason = if js_truthy(field(error, "recoverableViaImageStrip")) {
        "multimodal-processor-error"
    } else if string_field(error, "code").is_some_and(|s| s.equals_str("image_too_large")) {
        "image-too-large"
    } else {
        return None;
    };
    Some(object([
        ("type", "strip_images_and_retry".into()),
        ("reason", reason.into()),
    ]))
}
fn is_context_error(error: &JsValue) -> bool {
    string_field(error, "code")
        .is_some_and(|s| s.equals_str("prompt_too_long") || s.equals_str("context_overflow"))
        || field(error, "recoverableViaCompact") == &JsValue::Bool(true)
}
fn give_up(error: &JsValue) -> Result<JsValue, HostError> {
    let mut units: Vec<u16> = "non_recoverable_model_error:".encode_utf16().collect();
    units.extend(
        crate::js_value::to_string_units(field(error, "code"))
            .map_err(|_| ModuleError::new(crate::js_value::StackOverflow::MESSAGE))?,
    );
    Ok(object([
        ("type", "give_up".into()),
        ("reason", JsValue::String(JsString::from_utf16(units))),
    ]))
}
fn truncate(ratio: f64, reason: &str) -> JsValue {
    object([
        ("type", "truncate_head_and_retry".into()),
        ("keepRatio", JsValue::Number(ratio)),
        ("reason", reason.into()),
    ])
}
fn adjust(available: f64, scope: &str) -> JsValue {
    object([
        ("type", "adjust_output_and_retry".into()),
        ("maxOutputTokens", JsValue::Number(js_max(1.0, available))),
        ("reason", "provider-output-cap".into()),
        ("scope", scope.into()),
    ])
}
