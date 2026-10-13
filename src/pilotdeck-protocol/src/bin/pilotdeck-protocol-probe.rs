//! Diagnostic boundary executable, not a production AgentLoop or Gateway.
use pilotdeck_protocol::{
    JsObject, JsValue,
    module::{HOST_MODEL_METHODS, read_host_methods, validate_module_message},
    parse_json, stringify_json,
    wire::EnvelopeRef,
};
use std::io::{self, BufRead, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let action = std::env::args()
        .nth(1)
        .ok_or("required action: validate | envelope | roundtrip | methods | utf16 | clone")?;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        let value = parse_json(&line)?;
        let response = match action.as_str() {
            "validate" | "envelope" => {
                let result = if action == "validate" {
                    validate_module_message(&value)
                } else {
                    EnvelopeRef::decode(&value).map(|_| ())
                };
                let mut object = JsObject::new();
                object.insert("ok".into(), JsValue::Bool(result.is_ok()));
                if let Err(error) = result {
                    object.insert("code".into(), JsValue::from(error.code));
                    object.insert("message".into(), JsValue::String(error.message.into()));
                }
                JsValue::Object(object)
            }
            "methods" => JsValue::Array(
                read_host_methods(&value, HOST_MODEL_METHODS)
                    .into_iter()
                    .map(JsValue::String)
                    .collect(),
            ),
            "roundtrip" => value,
            "clone" => value.clone(),
            "utf16" => {
                let string = value.as_string().ok_or("UTF16 probe requires a string")?;
                let mut object = JsObject::new();
                object.insert("length".into(), JsValue::Number(string.len_utf16() as f64));
                object.insert(
                    "units".into(),
                    JsValue::Array(
                        string
                            .units()
                            .iter()
                            .map(|unit| JsValue::Number(f64::from(*unit)))
                            .collect(),
                    ),
                );
                JsValue::Object(object)
            }
            _ => return Err("unknown boundary probe action".into()),
        };
        let text = stringify_json(&response)?.ok_or("probe produced top-level undefined")?;
        writeln!(out, "{text}")?;
        out.flush()?;
    }
    Ok(())
}
