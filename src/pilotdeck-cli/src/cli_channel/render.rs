//! Rendering follows the fixed legacy CliChannel event contract.
use super::Failure;
use pilotdeck_protocol::{JsValue, number::number_to_string, stringify_json};
use std::io::{self, Write};

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn text(value: &JsValue) -> String {
    // Iterative Array#join preserves nested array semantics without stack overflow.
    enum Item<'a> {
        Value(&'a JsValue, bool),
        Comma,
    }
    let mut out = String::new();
    let mut pending = vec![Item::Value(value, false)];
    while let Some(item) = pending.pop() {
        match item {
            Item::Comma => out.push(','),
            Item::Value(JsValue::Array(values), _) => {
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(Item::Value(value, true));
                    if index > 0 {
                        pending.push(Item::Comma);
                    }
                }
            }
            Item::Value(JsValue::Undefined | JsValue::Null, true) => {}
            Item::Value(JsValue::Undefined, false) => out.push_str("undefined"),
            Item::Value(JsValue::Null, false) => out.push_str("null"),
            Item::Value(JsValue::Bool(value), _) => {
                out.push_str(if *value { "true" } else { "false" })
            }
            Item::Value(JsValue::Number(value), _) => out.push_str(&number_to_string(*value)),
            Item::Value(JsValue::String(value), _) => {
                out.push_str(&String::from_utf16_lossy(value.units()))
            }
            Item::Value(JsValue::Object(_), _) => out.push_str("[object Object]"),
        }
    }
    out
}

fn field(event: &JsValue, name: &str) -> String {
    text(event.get(name).unwrap_or(&JsValue::Undefined))
}
fn extra(event: &JsValue, name: &str) -> String {
    event
        .get(name)
        .filter(|v| truthy(v))
        .map(|v| format!(" {}", text(v)))
        .unwrap_or_default()
}
pub(super) fn write(event: &JsValue) -> Result<(), Failure> {
    let kind = event
        .get("type")
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
        .unwrap_or_default();
    let output = match kind.as_str() {
        "assistant_text_delta" => match event.get("text") {
            Some(JsValue::String(value)) => String::from_utf16_lossy(value.units()),
            None | Some(JsValue::Null | JsValue::Undefined) => String::new(),
            Some(value) if !truthy(value) => String::new(),
            Some(_) => return Err(Failure::Error("Invalid assistant text.".into())),
        },
        "assistant_thinking_delta" => format!("[thinking] {}", field(event, "text")),
        "tool_call_started" => format!(
            "\n[tool:{}] started{}\n",
            field(event, "name"),
            extra(event, "argsPreview")
        ),
        "tool_call_finished" => format!(
            "[tool:{}] {}{}\n",
            field(event, "toolCallId"),
            if event.get("ok").is_some_and(truthy) {
                "ok"
            } else {
                "error"
            },
            extra(event, "resultPreview")
        ),
        "permission_request" => format!(
            "[permission] {} requires approval ({})\n",
            field(event, "toolName"),
            field(event, "requestId")
        ),
        "structured_output" => {
            let undefined = JsValue::Undefined;
            let payload = event.get("payload").unwrap_or(&undefined);
            let json = stringify_json(payload)
                .map_err(|e| Failure::Error(e.to_string()))?
                .unwrap_or_else(|| "undefined".into());
            format!("\n[structured_output] {json}\n")
        }
        "plan_mode_changed" => format!("\n[mode] {}\n", field(event, "mode")),
        "error" => {
            let code = match event.get("code") {
                None | Some(JsValue::Null | JsValue::Undefined) => "gateway_error".to_owned(),
                Some(value) => text(value),
            };
            format!("\n[error:{code}] {}\n", field(event, "message"))
        }
        _ => String::new(),
    };
    if !output.is_empty() {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(output.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|error| Failure::Error(error.to_string()))?;
    }
    Ok(())
}
