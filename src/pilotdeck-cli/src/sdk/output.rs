//! Plain CLI projection over SDK terminal state; transport failures still throw.
use super::transport::string_field;
use crate::{Failure, stdout_line};
use pilotdeck_protocol::{JsObject, JsString, JsValue, number::number_to_string, stringify_json};
use std::io::{self, Write};

#[derive(Default)]
pub(super) struct TextOutput {
    chunks: Vec<Vec<u16>>,
    terminal: Option<Terminal>,
    structured: Option<JsValue>,
    session: String,
    run_id: JsString,
    streamed: bool,
}

enum Terminal {
    Completed(Option<JsValue>),
    Failed(JsValue),
    Unknown(JsString),
}

impl TextOutput {
    pub(super) fn new(session: &str, run_id: &str) -> Self {
        Self {
            session: session.into(),
            run_id: run_id.into(),
            ..Self::default()
        }
    }

    pub(super) fn event(&mut self, event: &JsValue) -> Result<(), Failure> {
        let kind = string_field(event, "type").unwrap_or_default();
        // SDK accumulates these before filtering partial assistant messages.
        if matches!(kind.as_str(), "assistant_text_delta" | "assistant.message") {
            if let Some(text) = event.get("text").and_then(JsValue::as_string) {
                self.chunks.push(text.units().to_vec());
            }
            return Ok(());
        }
        if matches!(
            kind.as_str(),
            "assistant_thinking_delta" | "assistant.thinking"
        ) {
            return Ok(());
        }
        if matches!(kind.as_str(), "turn_started" | "turn.started")
            && let Some(run_id) = event.get("runId").and_then(JsValue::as_string)
        {
            self.run_id = run_id.clone();
        }
        if kind == "structured_output" {
            self.structured = event.get("payload").cloned();
            return Ok(());
        }
        if kind == "turn_completed" {
            if self.terminal.is_none() {
                let output = self
                    .structured
                    .as_ref()
                    .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                    .cloned()
                    .or_else(|| {
                        (!self.chunks.is_empty())
                            .then(|| JsValue::String(JsString::from_utf16(self.chunks.concat())))
                    });
                self.terminal = Some(Terminal::Completed(output));
            }
            return Ok(());
        }
        if kind == "error" {
            if self.terminal.is_none() {
                let code = event
                    .get("code")
                    .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                    .map(js_string)
                    .transpose()?
                    .unwrap_or_else(|| "server_error".into());
                // Error.message is non-enumerable, but its String conversion
                // can still throw before a terminal result is constructed.
                if let Some(message) = event.get("message") {
                    js_string(message)?;
                }
                self.terminal = Some(if code.equals_str("result_unknown") {
                    Terminal::Unknown(self.run_id.clone())
                } else {
                    let details = event
                        .get("detail")
                        .filter(|value| matches!(value, JsValue::Object(_) | JsValue::Array(_)))
                        .cloned()
                        .unwrap_or_else(|| object([]));
                    Terminal::Failed(object([
                        ("code", JsValue::String(code)),
                        ("details", details),
                        ("retryable", JsValue::Bool(false)),
                        ("name", "PilotDeckError".into()),
                    ]))
                });
            }
            return Ok(());
        }
        // eventText checks the public type after SDK mapping. Unknown raw names
        // become pilotdeck.<name>, which retains the contains("assistant") rule.
        if kind.contains("assistant")
            || matches!(kind.as_str(), "subagent_text_delta" | "subagent.message")
        {
            let text = string_field(event, "text").or_else(|| string_field(event, "delta"));
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(text.as_bytes())
                    .and_then(|()| stdout.flush())
                    .map_err(|e| Failure::Error(e.to_string()))?;
                self.streamed = true;
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<u8, Failure> {
        match self
            .terminal
            .unwrap_or_else(|| Terminal::Unknown(self.run_id.clone()))
        {
            Terminal::Completed(output) => {
                if self.streamed {
                    stdout_line("");
                } else if let Some(output) = output {
                    print_value(&output)?;
                }
                Ok(0)
            }
            Terminal::Failed(error) => {
                print_value(&object([("status", "failed".into()), ("error", error)]))?;
                Ok(1)
            }
            Terminal::Unknown(run_id) => {
                print_value(&object([
                    ("status", "result_unknown".into()),
                    (
                        "recovery",
                        object([
                            ("sessionId", self.session.as_str().into()),
                            ("runId", JsValue::String(run_id)),
                        ]),
                    ),
                ]))?;
                Ok(1)
            }
        }
    }
}

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

fn print_value(value: &JsValue) -> Result<(), Failure> {
    let text = if let JsValue::String(text) = value {
        String::from_utf16_lossy(text.units())
    } else {
        stringify_json(value)
            .map_err(|e| Failure::Error(e.to_string()))?
            .unwrap_or_default()
    };
    stdout_line(&text);
    Ok(())
}

// String(error.code ?? "server_error"), preserving UTF-16 for JSON encoding.
// Use an explicit stack so deeply nested wire arrays cannot exhaust the stack.
fn js_string(value: &JsValue) -> Result<JsString, Failure> {
    let mut units = Vec::new();
    let comma = JsValue::from(",");
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            JsValue::String(text) => units.extend_from_slice(text.units()),
            JsValue::Array(values) => {
                for (index, value) in values.iter().enumerate().rev() {
                    if !matches!(value, JsValue::Null | JsValue::Undefined) {
                        pending.push(value);
                    }
                    if index > 0 {
                        pending.push(&comma);
                    }
                }
            }
            JsValue::Object(object) => {
                if object.get("toString").is_some() {
                    return Err(Failure::Error(
                        "Cannot convert object to primitive value".into(),
                    ));
                }
                units.extend("[object Object]".encode_utf16());
            }
            JsValue::Number(value) => units.extend(number_to_string(*value).encode_utf16()),
            JsValue::Bool(value) => units.extend(value.to_string().encode_utf16()),
            JsValue::Null => units.extend("null".encode_utf16()),
            JsValue::Undefined => units.extend("undefined".encode_utf16()),
        }
    }
    Ok(JsString::from_utf16(units))
}
