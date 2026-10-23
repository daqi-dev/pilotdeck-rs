//! Per-turn failure accounting, matching FailureGuard.ts and classify.ts at cc7c4e6.
//! A new guard resets the window; successful calls do not reset accumulated failures.
use std::collections::HashMap;

use pilotdeck_protocol::{JsString, JsValue};

use crate::metadata::object;

#[derive(Clone, Debug)]
pub enum GuardTrip {
    Tool {
        unit: JsString,
        label: JsString,
        failures: f64,
        limit: f64,
    },
    Model {
        failures: f64,
        limit: f64,
    },
}
impl GuardTrip {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Tool { .. } => "tool",
            Self::Model { .. } => "model",
        }
    }
    pub fn as_value(&self) -> JsValue {
        let mut value = object([("kind", self.kind().into())]);
        let JsValue::Object(fields) = &mut value else {
            unreachable!()
        };
        let (failures, limit) = match self {
            Self::Tool {
                unit,
                label,
                failures,
                limit,
            } => {
                fields.insert("unit".into(), JsValue::String(unit.clone()));
                fields.insert("label".into(), JsValue::String(label.clone()));
                (*failures, *limit)
            }
            Self::Model { failures, limit } => (*failures, *limit),
        };
        fields.insert("failures".into(), JsValue::Number(failures));
        fields.insert("limit".into(), JsValue::Number(limit));
        value
    }
}

pub struct FailureGuard {
    config: JsValue,
    tool_failures: HashMap<JsString, f64>,
    model_failures: f64,
    tripped: bool,
}
impl FailureGuard {
    /// `config` is the normalized runPolicy.failureGuard snapshot from the host.
    pub fn new(config: JsValue) -> Self {
        Self {
            config,
            tool_failures: HashMap::new(),
            model_failures: 0.0,
            tripped: false,
        }
    }
    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    pub fn on_tool_finished(
        &mut self,
        name: &JsString,
        ok: bool,
        code: Option<&JsString>,
    ) -> Option<GuardTrip> {
        if self.tripped || ok || !code.is_some_and(counted_tool_error_code) {
            return None;
        }
        let (unit, limit) = resolve_tool_unit(name, field(&self.config, "toolFailureLimits"))?;
        let failures = self.tool_failures.entry(unit.clone()).or_default();
        *failures += 1.0;
        if limit <= 0.0 || *failures < limit {
            return None;
        }
        self.tripped = true;
        Some(GuardTrip::Tool {
            label: tool_label(&unit, field(&self.config, "toolLabels")),
            unit,
            failures: *failures,
            limit,
        })
    }
    pub fn on_model_failure(&mut self) -> Option<GuardTrip> {
        if self.tripped {
            return None;
        }
        self.model_failures += 1.0;
        let limit = field(&self.config, "modelFailureLimit")
            .as_number()
            .unwrap_or(0.0);
        if limit <= 0.0 || self.model_failures < limit {
            return None;
        }
        self.tripped = true;
        Some(GuardTrip::Model {
            failures: self.model_failures,
            limit,
        })
    }
    pub fn observe(&mut self, event: &JsValue) -> Option<GuardTrip> {
        if is(event, "type", "tool_result") || is(event, "type", "subagent_tool_result") {
            let result = field(event, "result");
            if !is(result, "type", "error") {
                return None;
            }
            return self.on_tool_finished(
                field(result, "toolName").as_string()?,
                false,
                field(field(result, "error"), "code").as_string(),
            );
        }
        if is(event, "type", "turn_continued") && is(event, "reason", "model_error") {
            return self.on_model_failure();
        }
        None
    }
}

pub fn counted_tool_error_code(code: &JsString) -> bool {
    [
        "tool_execution_failed",
        "tool_unavailable",
        "setup_required",
    ]
    .iter()
    .any(|s| code.equals_str(s))
}
pub fn is_counted_tool_failure(result: &JsValue) -> bool {
    is(result, "type", "error")
        && field(field(result, "error"), "code")
            .as_string()
            .is_some_and(counted_tool_error_code)
}
pub fn resolve_tool_unit(name: &JsString, limits: &JsValue) -> Option<(JsString, f64)> {
    let limits = limits.as_object()?;
    if let Some(limit) = limits.get_key(name) {
        return Some((name.clone(), limit.as_number()?));
    }
    limits
        .entries()
        .iter()
        .filter(|(unit, _)| {
            unit.units().ends_with(&[95, 95]) && name.units().starts_with(unit.units())
        })
        .max_by_key(|(unit, _)| unit.units().len())
        .and_then(|(unit, limit)| Some((unit.clone(), limit.as_number()?)))
}
pub fn tool_label(unit: &JsString, labels: &JsValue) -> JsString {
    labels
        .as_object()
        .and_then(|labels| labels.get_key(unit))
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| unit.clone())
}
pub fn build_guard_trip_message(trip: &GuardTrip, model: Option<&JsValue>) -> JsValue {
    let (code, mut message, failures, mut detail) = match trip {
        GuardTrip::Tool {
            unit,
            label,
            failures,
            limit,
        } => (
            "tool_failure_limit_reached",
            label.units().to_vec(),
            *failures,
            object([
                ("unit", JsValue::String(unit.clone())),
                ("toolLabel", JsValue::String(label.clone())),
                ("failures", JsValue::Number(*failures)),
                ("limit", JsValue::Number(*limit)),
            ]),
        ),
        GuardTrip::Model { failures, limit } => (
            "model_failure_limit_reached",
            "模型调用".encode_utf16().collect(),
            *failures,
            object([
                ("failures", JsValue::Number(*failures)),
                ("limit", JsValue::Number(*limit)),
            ]),
        ),
    };
    if matches!(trip, GuardTrip::Tool { .. }) {
        message.extend(" 服务".encode_utf16());
    }
    message.extend(
        format!(
            "本次任务已失败 {} 次，已自动停止，请稍后重试",
            crate::js_value::number_string(failures)
        )
        .encode_utf16(),
    );
    if matches!(trip, GuardTrip::Model { .. })
        && let Some(model) = model
        && let JsValue::Object(detail) = &mut detail
    {
        for key in ["provider", "model"] {
            if let Some(text) = field(model, key)
                .as_string()
                .filter(|text| !text.is_empty())
            {
                detail.insert(key.into(), JsValue::String(text.clone()));
            }
        }
    }
    object([
        ("code", code.into()),
        ("message", JsValue::String(JsString::from_utf16(message))),
        ("detail", detail),
    ])
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn is(value: &JsValue, key: &str, text: &str) -> bool {
    field(value, key)
        .as_string()
        .is_some_and(|s| s.equals_str(text))
}

#[cfg(test)]
#[path = "failure_guard_tests.rs"]
mod tests;
