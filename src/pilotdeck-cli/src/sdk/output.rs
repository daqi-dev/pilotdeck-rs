//! CLI text/JSON/JSONL projection over SDK terminal state; transport failures still throw.
use super::transport::string_field;
use crate::{Failure, stdout_line};
use pilotdeck_protocol::{JsObject, JsString, JsValue, number::number_to_string, stringify_json};
use std::io::{self, Write};

pub(super) struct TextOutput {
    chunks: Vec<Vec<u16>>,
    terminal: Option<Terminal>,
    structured: Option<JsValue>,
    session: String,
    run_id: JsString,
    streamed: bool,
    stream_json: bool,
    json: bool,
    usage: JsValue,
    finish_reason: JsValue,
    sequence: f64,
}

/// SDK iterator failures retain the query; CLI loop-body failures invoke
/// AsyncIteratorClose and therefore close it.
#[derive(Debug)]
pub(super) enum EventError {
    Iterator(Failure),
    Consumer(Failure),
}

impl From<Failure> for EventError {
    fn from(error: Failure) -> Self {
        Self::Iterator(error)
    }
}

enum Terminal {
    Completed(Option<JsValue>),
    Failed(JsValue),
    Unknown(JsString),
}

impl TextOutput {
    pub(super) fn new(session: &str, run_id: &str, json: bool, stream_json: bool) -> Self {
        Self {
            chunks: Vec::new(),
            terminal: None,
            structured: None,
            session: session.into(),
            run_id: run_id.into(),
            streamed: false,
            stream_json,
            json,
            usage: JsValue::Object(JsObject::new()),
            finish_reason: JsValue::Undefined,
            sequence: 0.0,
        }
    }

    pub(super) fn run_id(&self) -> &JsString {
        &self.run_id
    }

    pub(super) fn event(&mut self, event: &JsValue) -> Result<(), EventError> {
        // Transport::next_sequence requires contiguous frames starting at zero
        // and drops duplicates before yielding. Thus the accepted-event ordinal
        // is exactly frame.seq, including events hidden by the SDK below.
        // Apply the SDK transport's spread/sequence projection at this boundary
        // without changing the shared RPC transport surface.
        let projected = sequenced_event(event, self.sequence);
        self.sequence += 1.0;
        let event = &projected;
        // mapPublicMessage runs before next() filters partial assistant
        // messages, so the type conversion applies to every event.
        let raw_type = match event.get("type") {
            Some(value) if !matches!(value, JsValue::Null | JsValue::Undefined) => {
                js_string(value)?
            }
            _ => JsString::from("unknown"),
        };
        // SDK accumulates these before filtering partial assistant messages.
        if raw_type.equals_str("assistant_text_delta") || raw_type.equals_str("assistant.message") {
            if let Some(text) = event.get("text").and_then(JsValue::as_string) {
                self.chunks.push(text.units().to_vec());
            }
            return Ok(());
        }
        if raw_type.equals_str("assistant_thinking_delta")
            || raw_type.equals_str("assistant.thinking")
        {
            return Ok(());
        }
        if (raw_type.equals_str("turn_started") || raw_type.equals_str("turn.started"))
            && let Some(run_id) = event.get("runId").and_then(JsValue::as_string)
        {
            self.run_id = run_id.clone();
        }
        if raw_type.equals_str("structured_output") {
            self.structured = event.get("payload").cloned();
        }
        // next() suppresses a duplicate result/error before any other handling.
        if self.terminal.is_some()
            && (raw_type.equals_str("turn_completed") || raw_type.equals_str("error"))
        {
            return Ok(());
        }
        if raw_type.equals_str("turn_completed") {
            let output = self
                .structured
                .as_ref()
                .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                .cloned()
                .or_else(|| {
                    (!self.chunks.is_empty())
                        .then(|| JsValue::String(JsString::from_utf16(self.chunks.concat())))
                });
            self.usage = as_record(event.get("usage"));
            self.finish_reason = event
                .get("finishReason")
                .cloned()
                .unwrap_or(JsValue::Undefined);
            self.terminal = Some(Terminal::Completed(output));
        }
        if raw_type.equals_str("error") {
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
        if self.stream_json {
            // consumeRun prints every public event the SDK yields.
            let mapped = public_event(event, &raw_type, &self.session);
            let text = stringify_json(&mapped)
                .map_err(|e| EventError::Consumer(Failure::Error(e.to_string())))?
                .unwrap_or_default();
            stdout_line(&text);
            return Ok(());
        }
        if !matches!(public_type(&raw_type, event), JsValue::String(_)) {
            return Err(EventError::Consumer(Failure::Error(
                "event.type.includes is not a function".into(),
            )));
        }
        // eventText checks the public type after SDK mapping. Unknown raw names
        // become pilotdeck.<name>, which retains the contains("assistant") rule.
        if contains_sub(&raw_type, "assistant")
            || raw_type.equals_str("subagent_text_delta")
            || raw_type.equals_str("subagent.message")
        {
            let text = string_field(event, "text").or_else(|| string_field(event, "delta"));
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(text.as_bytes())
                    .and_then(|()| stdout.flush())
                    .map_err(|e| EventError::Consumer(Failure::Error(e.to_string())))?;
                self.streamed = true;
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<u8, Failure> {
        let terminal = self
            .terminal
            .unwrap_or_else(|| Terminal::Unknown(self.run_id.clone()));
        if self.json {
            // consumeRun emits the streamed-text newline before the envelope;
            // only a completed run streams a trailing newline.
            let completed = matches!(terminal, Terminal::Completed(_));
            if completed && self.streamed {
                stdout_line("");
            }
            let result = match terminal {
                Terminal::Completed(output) => object([
                    ("status", "completed".into()),
                    ("output", output.unwrap_or(JsValue::Undefined)),
                    ("usage", self.usage),
                    ("finishReason", self.finish_reason),
                ]),
                Terminal::Failed(error) => object([("status", "failed".into()), ("error", error)]),
                Terminal::Unknown(run_id) => object([
                    ("status", "result_unknown".into()),
                    (
                        "recovery",
                        object([
                            ("sessionId", self.session.as_str().into()),
                            ("runId", JsValue::String(run_id)),
                        ]),
                    ),
                ]),
            };
            print_value(&object([
                ("sessionId", self.session.as_str().into()),
                ("result", result),
            ]))?;
            return Ok(u8::from(!completed));
        }
        match terminal {
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

// { ...frame.event, sequence: frame.seq }: own fields retain insertion order,
// arrays/strings spread numeric keys, and UTF-16 string indices retain lone
// surrogates until JSON encoding. A wire sequence property is always replaced.
fn sequenced_event(event: &JsValue, sequence: f64) -> JsValue {
    let mut projected = JsObject::new();
    match event {
        JsValue::Object(value) => {
            for (key, value) in value.entries() {
                projected.insert(key.clone(), value.clone());
            }
        }
        JsValue::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                projected.insert(index.to_string().into(), value.clone());
            }
        }
        JsValue::String(value) => {
            for (index, unit) in value.units().iter().enumerate() {
                projected.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        _ => {}
    }
    projected.insert("sequence".into(), JsValue::Number(sequence));
    JsValue::Object(projected)
}

// mapPublicMessage uses a normal object literal for names. Its inherited
// functions disappear during JSON serialization; __proto__ serializes as {}.
// Both remain non-string values for the CLI's eventText type check.
fn public_type(raw: &JsString, event: &JsValue) -> JsValue {
    if raw.equals_str("__proto__") {
        return object([]);
    }
    if [
        "constructor",
        "toString",
        "toLocaleString",
        "valueOf",
        "hasOwnProperty",
        "isPrototypeOf",
        "propertyIsEnumerable",
        "__defineGetter__",
        "__defineSetter__",
        "__lookupGetter__",
        "__lookupSetter__",
    ]
    .iter()
    .any(|name| raw.equals_str(name))
    {
        return JsValue::Undefined;
    }
    let name = if raw.equals_str("turn_started") {
        "turn.started"
    } else if raw.equals_str("input_accepted") {
        "user.accepted"
    } else if raw.equals_str("assistant_text_delta") {
        "assistant.message"
    } else if raw.equals_str("assistant_thinking_delta") {
        "assistant.thinking"
    } else if raw.equals_str("subagent_text_delta") {
        "subagent.message"
    } else if raw.equals_str("tool_call_started") {
        "tool.started"
    } else if raw.equals_str("tool_progress") {
        "tool.progress"
    } else if raw.equals_str("prompt_suggestion") {
        "prompt_suggestion"
    } else if raw.equals_str("tool_call_finished") {
        if event.get("ok") == Some(&JsValue::Bool(false)) {
            "tool.failed"
        } else {
            "tool.completed"
        }
    } else if raw.equals_str("tool_result_detail_available") {
        "tool.result_detail"
    } else if raw.equals_str("permission_request") {
        "permission.requested"
    } else if raw.equals_str("hook_started") {
        "hook.started"
    } else if raw.equals_str("hook_response") {
        "hook.response"
    } else if raw.equals_str("hook_async_result") {
        "hook.async_result"
    } else if raw.equals_str("elicitation_request") {
        "elicitation.requested"
    } else if raw.equals_str("elicitation_cancelled") {
        "elicitation.cancelled"
    } else if raw.equals_str("user_dialog_request") {
        "user_dialog.requested"
    } else if raw.equals_str("user_dialog_cancelled") {
        "user_dialog.cancelled"
    } else if raw.equals_str("structured_output") {
        "structured_output"
    } else if raw.equals_str("context_budget") {
        "context.usage"
    } else if raw.equals_str("turn_completed") {
        "result"
    } else if raw.equals_str("error") {
        "error"
    } else {
        ""
    };
    if !name.is_empty() {
        return JsValue::from(name);
    }
    if raw.equals_str("permission_denied") {
        return JsValue::from("permission.denied");
    }
    if contains_sub(raw, ".") {
        JsValue::String(raw.clone())
    } else {
        let mut units = "pilotdeck.".encode_utf16().collect::<Vec<u16>>();
        units.extend_from_slice(raw.units());
        JsValue::String(JsString::from_utf16(units))
    }
}

// { type, ...event, sessionId, runId, sequence } plus the terminal and
// context-budget extensions. First insertion owns key position; the final
// type assignment replaces the placeholder value in place.
fn public_event(event: &JsValue, raw_type: &JsString, session: &str) -> JsValue {
    let mut object = JsObject::new();
    object.insert("type".into(), JsValue::String(raw_type.clone()));
    if let Some(raw) = event.as_object() {
        for (key, value) in raw.entries() {
            object.insert(key.clone(), value.clone());
        }
    }
    object.insert("sessionId".into(), JsValue::String(JsString::from(session)));
    object.insert(
        "runId".into(),
        event.get("runId").cloned().unwrap_or(JsValue::Undefined),
    );
    object.insert(
        "sequence".into(),
        event.get("sequence").cloned().unwrap_or(JsValue::Undefined),
    );
    if raw_type.equals_str("turn_completed") {
        object.insert("status".into(), "completed".into());
        object.insert(
            "output".into(),
            event.get("result").cloned().unwrap_or(JsValue::Undefined),
        );
        object.insert(
            "finishReason".into(),
            event
                .get("finishReason")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
    }
    if raw_type.equals_str("error") {
        object.insert("status".into(), "failed".into());
    }
    if raw_type.equals_str("context_budget") {
        let mut usage = JsObject::new();
        for key in CONTEXT_KEYS {
            if let Some(value) = event.get(key).filter(|value| !value.is_undefined()) {
                usage.insert((*key).into(), value.clone());
            }
        }
        object.insert("usage".into(), JsValue::Object(usage));
    }
    object.insert("type".into(), public_type(raw_type, event));
    JsValue::Object(object)
}

// The compact legacy usage projection of context-budget events.
const CONTEXT_KEYS: [&str; 24] = [
    "used",
    "displayUsed",
    "budgetUsed",
    "localEstimateTokens",
    "displayTokens",
    "estimateSource",
    "usageTokens",
    "calibrationActualInputTokens",
    "calibrationEstimatedInputTokens",
    "total",
    "totalContextTokens",
    "maxContextTokens",
    "effectiveTotal",
    "effectiveContextTokens",
    "maxOutputTokens",
    "reservedOutputTokens",
    "warningRatio",
    "blockingRatio",
    "ratio",
    "state",
    "source",
    "exact",
    "estimatorError",
    "breakdown",
];

fn contains_sub(value: &JsString, needle: &str) -> bool {
    let needle = needle.encode_utf16().collect::<Vec<u16>>();
    value
        .units()
        .windows(needle.len())
        .any(|window| window == needle.as_slice())
}

// asRecord: objects and arrays pass through, anything else becomes {}.
fn as_record(value: Option<&JsValue>) -> JsValue {
    value
        .filter(|value| matches!(value, JsValue::Object(_) | JsValue::Array(_)))
        .cloned()
        .unwrap_or_else(|| object([]))
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
