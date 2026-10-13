//! Source AgentLoop budget terminal projection; no charging or effects.
use crate::budget_terminal::{BudgetTerminalInput, BudgetTerminalPlan};
use crate::driver::BudgetStop;
use crate::ports::{PortError, PortResult};
use crate::turn::TurnStopReason;
use crate::{MessageProjection, ProjectedMessage, ProjectionGap};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn source_exception(message: &str, input: &JsValue) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(&object([
            ("name", "TypeError".into()),
            ("message", JsValue::String(message.into())),
            ("input", input.clone()),
        ])),
    }
}
fn property<'a>(value: &'a JsValue, name: &str) -> PortResult<Option<&'a JsValue>> {
    match value {
        JsValue::Null => Err(source_exception(
            &format!("Cannot read properties of null (reading '{name}')"),
            value,
        )),
        JsValue::Undefined => Err(source_exception(
            &format!("Cannot read properties of undefined (reading '{name}')"),
            value,
        )),
        _ => Ok(value.get(name)),
    }
}
fn number_text(number: f64) -> PortResult<String> {
    if number.is_nan() {
        return Ok("NaN".into());
    }
    if number == f64::INFINITY {
        return Ok("Infinity".into());
    }
    if number == f64::NEG_INFINITY {
        return Ok("-Infinity".into());
    }
    // Accepted protocol codec uses ryu-js Number::toString, not Rust Display.
    stringify_json(&JsValue::Number(number))
        .map_err(|error| PortError::InvalidResponse {
            message: error.to_string().into(),
        })?
        .ok_or_else(|| PortError::InvalidResponse {
            message: "Number formatter returned undefined.".into(),
        })
}
/// Exact binary rational then nearest integer with ties up (ECMA262 toFixed).
/// For |x|<1e21, x*1e6<1e27 (<2^90); significand*1e6<2^73.
/// Those bounds make positive shifts and u128 remainder arithmetic safe.
pub(crate) fn to_fixed_6(number: f64) -> PortResult<String> {
    if !number.is_finite() || number.abs() >= 1e21 {
        return number_text(number);
    }
    let sign = if number < 0.0 { "-" } else { "" };
    let bits = number.abs().to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (significand, exponent) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1_u64 << 52), biased - 1023 - 52)
    };
    let scaled = u128::from(significand) * 1_000_000;
    let integer = if exponent >= 0 {
        scaled << exponent as u32
    } else {
        let shift = (-exponent) as u32;
        if shift >= 128 {
            0
        } else {
            let divisor = 1_u128 << shift;
            let quotient = scaled / divisor;
            let remainder = scaled % divisor;
            quotient + u128::from(remainder >= divisor / 2)
        }
    };
    Ok(format!(
        "{sign}{}.{:06}",
        integer / 1_000_000,
        integer % 1_000_000
    ))
}
/// Array.join nullish elements are empty. Iterative traversal avoids imposing
/// an invented recursion limit on ordinary lossless values. Native callable
/// methods, accessors, symbols and prototype graphs are outside JsValue.
fn join_text(value: &JsValue) -> PortResult<JsString> {
    enum Part<'a> {
        Value(&'a JsValue),
        Comma,
    }
    let mut parts = vec![Part::Value(value)];
    let mut units = Vec::new();
    while let Some(part) = parts.pop() {
        match part {
            Part::Comma => units.push(u16::from(b',')),
            Part::Value(value) => match value {
                JsValue::Null | JsValue::Undefined => {}
                JsValue::String(value) => units.extend_from_slice(value.units()),
                JsValue::Bool(value) => {
                    units.extend(if *value { "true" } else { "false" }.encode_utf16())
                }
                JsValue::Number(value) => units.extend(number_text(*value)?.encode_utf16()),
                JsValue::Array(values) => {
                    for (index, value) in values.iter().enumerate().rev() {
                        parts.push(Part::Value(value));
                        if index > 0 {
                            parts.push(Part::Comma);
                        }
                    }
                }
                JsValue::Object(object) => {
                    // Own noncallable data toString shadows Object.prototype.
                    // Inherited valueOf returns an object, so conversion fails.
                    if object.contains_key("toString") {
                        return Err(source_exception(
                            "Cannot convert object to primitive value",
                            value,
                        ));
                    }
                    units.extend("[object Object]".encode_utf16());
                }
            },
        }
    }
    Ok(JsString::from_utf16(units))
}
fn js_space(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn safe_message(
    message: &ProjectedMessage,
    unexecuted: usize,
) -> PortResult<Option<ProjectedMessage>> {
    if unexecuted > 0 {
        return Ok(None);
    }
    let raw = message.boundary.value();
    let Some(JsValue::Array(blocks)) = property(raw, "content")? else {
        return Ok(None);
    };
    let mut filtered = Vec::new();
    // Source filters thinking across ALL blocks before its text/coercion pass.
    for block in blocks {
        if !property(block, "type")?
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("thinking"))
        {
            filtered.push(block.clone());
        }
    }
    let mut joined = Vec::new();
    let mut text_count = 0;
    for block in &filtered {
        if property(block, "type")?
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("text"))
        {
            if text_count > 0 {
                joined.push(u16::from(b'\n'));
            }
            text_count += 1;
            joined.extend_from_slice(
                join_text(property(block, "text")?.unwrap_or(&JsValue::Undefined))?.units(),
            );
        }
    }
    if joined.iter().all(|unit| js_space(*unit)) {
        return Ok(None);
    }
    // Successful content implies an object in this admitted own-property domain.
    let Some(raw_object) = raw.as_object() else {
        return Ok(None);
    };
    let mut output = raw_object.clone();
    output.insert("content".into(), JsValue::Array(filtered));
    Ok(Some(ProjectedMessage {
        boundary: BoundarySnapshot::capture(&JsValue::Object(output)),
        // No unchecked reuse of a possibly contradictory normalized view.
        normalized: MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    }))
}
pub(crate) fn evaluate(input: BudgetTerminalInput<'_>) -> PortResult<BudgetTerminalPlan> {
    let ledger = input.ledger;
    let turn_reached = ledger
        .turn_limit_usd
        .is_some_and(|limit| ledger.turn_spent_usd >= limit);
    let task_reached = ledger
        .task_limit_usd
        .is_some_and(|limit| ledger.task_spent_usd >= limit);
    let selected = if turn_reached {
        Some(BudgetStop::Turn)
    } else if task_reached {
        Some(BudgetStop::Task)
    } else {
        None
    };
    // Target caller invariant, explicitly distinct from Source input validation.
    if selected != Some(input.stop) {
        return Err(PortError::InvalidResponse {
            message: "Budget terminal stop does not match the already charged ledger.".into(),
        });
    }
    let (limit, code, name, event, hint, reason) = match input.stop {
        BudgetStop::Turn => (
            ledger.turn_limit_usd,
            "agent_max_budget_reached",
            "maxBudgetUsd",
            "max_budget_reached",
            "Increase maxBudgetUsd or start a new turn with a larger budget.",
            TurnStopReason::MaxBudget,
        ),
        BudgetStop::Task => (
            ledger.task_limit_usd,
            "agent_task_budget_reached",
            "taskBudget.total",
            "task_budget_reached",
            "Increase taskBudget.total or start a new SDK session with a larger budget.",
            TurnStopReason::TaskBudget,
        ),
    };
    let limit = limit.ok_or_else(|| PortError::InvalidResponse {
        message: "Budget terminal limit is absent.".into(),
    })?;
    let message = format!(
        "Reached Gateway-owned {name} (${}) after spending ${}.",
        to_fixed_6(limit)?,
        to_fixed_6(ledger.task_spent_usd)?
    );
    let mut details = JsObject::new();
    if let Some(limit) = ledger.turn_limit_usd {
        details.insert("maxBudgetUsd".into(), JsValue::Number(limit));
    }
    if let Some(limit) = ledger.task_limit_usd {
        details.insert("taskBudgetUsd".into(), JsValue::Number(limit));
    }
    details.insert(
        "spentBudgetUsd".into(),
        JsValue::Number(ledger.task_spent_usd),
    );
    details.insert(
        "turnSpentBudgetUsd".into(),
        JsValue::Number(ledger.turn_spent_usd),
    );
    details.insert(
        "lastInvocationCostUsd".into(),
        input
            .last_invocation_cost_usd
            .map_or(JsValue::Undefined, JsValue::Number),
    );
    details.insert(
        "provider".into(),
        JsValue::String(input.route.provider.clone()),
    );
    details.insert("model".into(), JsValue::String(input.route.model.clone()));
    let details = JsValue::Object(details);
    let error = object([
        ("code", code.into()),
        ("message", JsValue::String(message.into())),
        ("details", details.clone()),
        ("userHint", hint.into()),
    ]);
    Ok(BudgetTerminalPlan {
        error: BoundarySnapshot::capture(&error),
        status_event: event.into(),
        status_detail: BoundarySnapshot::capture(&details),
        safe_message: safe_message(input.assistant_message, input.unexecuted_tool_call_count)?,
        stop_reason: reason,
    })
}
